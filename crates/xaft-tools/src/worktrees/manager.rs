//! Safe Git worktree lifecycle primitives.
//!
//! Port of `agenthicc.worktrees.manager.WorktreeManager`. Every operation shells
//! out to the system `git`, exactly as agenthicc does with `subprocess`, and
//! every one of agenthicc's safety rules is kept:
//!
//! * a worktree is created only from a **clean** coordinator tree, at a
//!   validated object id, on a branch name `git check-ref-format` accepts;
//! * a merge is attempted only when the coordinator is still on the branch the
//!   orchestration recorded, and a conflicting merge is **aborted** so the
//!   coordinator is left as it was while the worker branch is preserved;
//! * a worktree is removed only once integrated and clean, unless `force`;
//! * `recover` reports Git's own view (`git worktree list --porcelain`), so a
//!   crash between "Git created it" and "the manifest recorded it" is
//!   reconcilable.
//!
//! Completion evidence ([`WorkerResult`]) is derived from Git only — commits,
//! changed files, cleanliness — never from what a worker says it did.
//!
//! # Where worktrees live
//!
//! `$XAFT_HOME/worktrees/<repository name>/<worktree id>` (default
//! `~/.xaft/worktrees/…`), agenthicc's `~/.agenthicc/worktrees/<name>` under
//! xaft's home. Branches are named `xaft/<session>/<task>-<id8>`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::error::{WorktreeError, truncate};
use super::model::{WorkerResult, WorktreeRecord, WorktreeStatus, now_secs};

/// Default ceiling for one `git` call (agenthicc: 60 s).
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Ceiling for `git merge` / `git rebase` (agenthicc: 120 s).
const MERGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Outcome of integrating (or rebasing) a worker branch.
///
/// Serialises with exactly agenthicc's `IntegrationResult.__dict__` keys, which
/// `worktrees integrate` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationResult {
    /// Whether the merge (or rebase) succeeded.
    pub ok: bool,
    /// The coordinator repository.
    pub repository: String,
    /// The worker branch.
    pub branch: String,
    /// The resulting HEAD, when `ok`.
    pub head_commit: String,
    /// Paths in conflict, when not `ok`.
    pub conflict_paths: Vec<String>,
    /// Git's own output, truncated.
    pub message: String,
}

/// Machine-derived state of one worktree.
#[derive(Debug, Clone, PartialEq)]
pub struct WorktreeInspection {
    /// The record, with `head_commit` and `dirty` refreshed.
    pub record: WorktreeRecord,
    /// Completion evidence.
    pub result: WorkerResult,
    /// `git diff base..head`.
    pub diff: String,
}

/// Create, inspect, integrate, recover and safely remove worktrees.
#[derive(Debug, Clone)]
pub struct WorktreeManager {
    repository: PathBuf,
    root: PathBuf,
}

/// Captured output of one finished command.
struct Finished {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run `command` to completion or until `timeout`, capturing both streams.
///
/// The streams are drained on their own threads so a chatty `git` cannot
/// deadlock on a full pipe while this thread waits for it to exit.
fn run_bounded(mut command: Command, timeout: Duration) -> std::io::Result<Finished> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out after {}s", timeout.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Ok(Finished {
        success: status.success(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// Replace every run of characters outside `[A-Za-z0-9._-]` with `-`, trim
/// `-`/`.` from the ends and cap at 48 characters (agenthicc's `_slug`).
fn slug(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in value.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    let chosen = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    chosen.chars().take(48).collect()
}

fn valid_worktree_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_object_id(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn first_line(output: &str) -> String {
    output.lines().next().unwrap_or("").trim().to_string()
}

fn non_empty_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

impl WorktreeManager {
    /// A manager for the repository containing `repository`, with worktrees
    /// under `$XAFT_HOME/worktrees/<repository name>`.
    pub fn new(repository: &Path) -> Result<Self, WorktreeError> {
        let resolved = Self::resolve_repository(repository)?;
        let name = resolved
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".into());
        let home = crate::trust::xaft_home().ok_or_else(|| {
            WorktreeError::Io(std::io::Error::other(
                "cannot determine home directory; set XAFT_HOME",
            ))
        })?;
        Self::with_root_resolved(resolved, home.join("worktrees").join(name))
    }

    /// A manager with worktrees under an explicit `root`.
    pub fn with_root(repository: &Path, root: &Path) -> Result<Self, WorktreeError> {
        let resolved = Self::resolve_repository(repository)?;
        Self::with_root_resolved(resolved, root.to_path_buf())
    }

    fn with_root_resolved(repository: PathBuf, root: PathBuf) -> Result<Self, WorktreeError> {
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize().unwrap_or(root);
        Ok(Self { repository, root })
    }

    fn resolve_repository(path: &Path) -> Result<PathBuf, WorktreeError> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"]);
        let finished = run_bounded(command, Duration::from_secs(15))
            .map_err(|_| WorktreeError::Repository(path.display().to_string()))?;
        if !finished.success {
            return Err(WorktreeError::Repository(path.display().to_string()));
        }
        let raw = first_line(&finished.stdout);
        if raw.is_empty() {
            return Err(WorktreeError::Repository(path.display().to_string()));
        }
        let root = PathBuf::from(raw);
        Ok(root.canonicalize().unwrap_or(root))
    }

    /// The resolved repository root.
    pub fn repository(&self) -> &Path {
        &self.repository
    }

    /// Where worktrees are created.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn git(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        check: bool,
        timeout: Duration,
    ) -> Result<String, WorktreeError> {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd.unwrap_or(&self.repository));
        let finished = run_bounded(command, timeout).map_err(|e| WorktreeError::Git {
            command: args.join(" "),
            output: format!("failed to start: {e}"),
        })?;
        let output = format!("{}{}", finished.stdout, finished.stderr)
            .trim()
            .to_string();
        if check && !finished.success {
            return Err(WorktreeError::Git {
                command: args.join(" "),
                output: truncate(&output, 1000),
            });
        }
        Ok(output)
    }

    fn run(&self, args: &[&str], cwd: Option<&Path>) -> Result<String, WorktreeError> {
        self.git(args, cwd, true, GIT_TIMEOUT)
    }

    /// `git rev-parse HEAD` in `cwd` (default: the repository).
    pub fn head(&self, cwd: Option<&Path>) -> Result<String, WorktreeError> {
        Ok(first_line(&self.run(&["rev-parse", "HEAD"], cwd)?))
    }

    /// The checked-out branch in `cwd`.
    pub fn branch(&self, cwd: Option<&Path>) -> Result<String, WorktreeError> {
        Ok(first_line(
            &self.run(&["symbolic-ref", "--short", "HEAD"], cwd)?,
        ))
    }

    /// Whether `cwd` has no uncommitted changes.
    ///
    /// For the coordinator repository itself, changes under this manager's own
    /// worktree root are ignored when that root lives inside the repository —
    /// it is infrastructure, not user work. Unrelated untracked files still
    /// count, so the coordinator refuses to start on a dirty tree.
    pub fn is_clean(&self, cwd: Option<&Path>) -> Result<bool, WorktreeError> {
        let output = self.run(&["status", "--porcelain"], cwd)?;
        if output.is_empty() {
            return Ok(true);
        }
        let target = cwd
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
            .unwrap_or_else(|| self.repository.clone());
        if target != self.repository {
            return Ok(false);
        }
        let ignored = self
            .root
            .strip_prefix(&self.repository)
            .ok()
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .filter(|rel| !rel.is_empty());
        let remaining = output.lines().filter(|line| {
            let changed = if line.len() >= 4 {
                line[3..].trim()
            } else {
                ""
            };
            match &ignored {
                Some(root) => {
                    let root = root.trim_end_matches('/');
                    !(changed == root
                        || changed.trim_end_matches('/') == root
                        || changed.starts_with(&format!("{root}/")))
                }
                None => true,
            }
        });
        Ok(remaining.count() == 0)
    }

    /// Fail with [`WorktreeError::Dirty`] unless `cwd` is clean.
    pub fn ensure_clean(&self, cwd: Option<&Path>) -> Result<(), WorktreeError> {
        if self.is_clean(cwd)? {
            return Ok(());
        }
        Err(WorktreeError::Dirty(
            cwd.unwrap_or(&self.repository).display().to_string(),
        ))
    }

    /// Resolve `base_commit` to a full commit id, refusing non-object-ids.
    pub fn validate_base(&self, base_commit: &str) -> Result<String, WorktreeError> {
        if !is_object_id(base_commit) {
            return Err(WorktreeError::Invalid(
                "base_commit must be a Git object id".into(),
            ));
        }
        Ok(first_line(&self.run(
            &["rev-parse", &format!("{base_commit}^{{commit}}")],
            None,
        )?))
    }

    /// Create one branch and worktree from a clean, immutable base.
    ///
    /// Re-adopts an existing path only when both its root and its branch match
    /// the requested identity exactly — the crash-recovery case — and refuses
    /// anything else.
    pub fn create(
        &self,
        parent_session_id: &str,
        task_id: &str,
        base_commit: Option<&str>,
        worktree_id: Option<&str>,
        branch_name: Option<&str>,
    ) -> Result<WorktreeRecord, WorktreeError> {
        let head;
        let base = match base_commit {
            Some(base) => self.validate_base(base)?,
            None => {
                head = self.head(None)?;
                self.validate_base(&head)?
            }
        };
        let stable_id = match worktree_id {
            Some(id) => id.to_string(),
            None => uuid::Uuid::new_v4().simple().to_string()[..16].to_string(),
        };
        if !valid_worktree_id(&stable_id) {
            return Err(WorktreeError::Invalid(
                "worktree_id contains unsupported characters".into(),
            ));
        }
        let branch = match branch_name {
            Some(name) => name.to_string(),
            None => format!(
                "xaft/{}/{}-{}",
                slug(parent_session_id, "session"),
                slug(task_id, "task"),
                &stable_id[..stable_id.len().min(8)]
            ),
        };
        let mut probe = Command::new("git");
        probe
            .args(["check-ref-format", "--branch", &branch])
            .current_dir(&self.repository);
        let valid_branch = run_bounded(probe, Duration::from_secs(15))
            .map(|f| f.success)
            .unwrap_or(false);
        if !valid_branch {
            return Err(WorktreeError::Invalid(
                "branch_name is not a valid Git branch".into(),
            ));
        }

        let path = self.root.join(&stable_id);
        let now = now_secs();
        let mut record = WorktreeRecord {
            worktree_id: stable_id.clone(),
            repository: self.repository.display().to_string(),
            path: path.display().to_string(),
            branch: branch.clone(),
            base_commit: base.clone(),
            owner_session_id: parent_session_id.to_string(),
            task_id: task_id.to_string(),
            status: WorktreeStatus::Creating,
            head_commit: String::new(),
            dirty: false,
            conflict_paths: Vec::new(),
            created_at: now,
            updated_at: now,
            last_error: String::new(),
            environment_receipts: Vec::new(),
        };

        if path.exists() {
            let mut probe = Command::new("git");
            probe
                .arg("-C")
                .arg(&path)
                .args(["rev-parse", "--show-toplevel"]);
            let actual_root = run_bounded(probe, Duration::from_secs(15))
                .ok()
                .filter(|f| f.success)
                .map(|f| PathBuf::from(first_line(&f.stdout)))
                .ok_or_else(|| {
                    WorktreeError::Invalid(format!(
                        "existing worktree path is not recoverable: {}",
                        path.display()
                    ))
                })?;
            let actual_root = actual_root.canonicalize().unwrap_or(actual_root);
            let actual_branch = self.branch(Some(&path)).map_err(|_| {
                WorktreeError::Invalid(format!(
                    "existing worktree path is not recoverable: {}",
                    path.display()
                ))
            })?;
            let expected = path.canonicalize().unwrap_or_else(|_| path.clone());
            if actual_root != expected || actual_branch != branch {
                return Err(WorktreeError::Invalid(format!(
                    "existing worktree does not match its recovery identity: {}",
                    path.display()
                )));
            }
            record.status = WorktreeStatus::Active;
            record.head_commit = self.head(Some(&path))?;
            record.dirty = !self.is_clean(Some(&path))?;
            return Ok(record);
        }

        // A branch that already existed is never deleted on failure: it is
        // not this call's to clean up. agenthicc deletes it unconditionally,
        // which can destroy a recovery branch it was asked to re-adopt.
        let branch_existed = self.branch_exists(&branch);
        let attempt = (|| -> Result<(), WorktreeError> {
            self.ensure_clean(None)?;
            let path_text = path.display().to_string();
            if branch_existed {
                let branch_head = first_line(&self.run(
                    &["rev-parse", &format!("refs/heads/{branch}^{{commit}}")],
                    None,
                )?);
                if branch_head != base {
                    return Err(WorktreeError::Invalid(
                        "recovery branch exists at a different commit".into(),
                    ));
                }
                self.run(&["worktree", "add", &path_text, &branch], None)?;
            } else {
                self.run(&["worktree", "add", "-b", &branch, &path_text, &base], None)?;
            }
            Ok(())
        })();
        if let Err(e) = attempt {
            // Remove only the exact path and branch this call allocated.
            if path.exists() {
                let path_text = path.display().to_string();
                let _ = self.git(
                    &["worktree", "remove", "--force", &path_text],
                    None,
                    false,
                    GIT_TIMEOUT,
                );
            }
            if !branch_existed {
                let _ = self.git(&["branch", "-D", &branch], None, false, GIT_TIMEOUT);
            }
            return Err(e);
        }
        record.status = WorktreeStatus::Active;
        record.head_commit = self.head(Some(&path))?;
        record.dirty = false;
        record.updated_at = now_secs();
        Ok(record)
    }

    fn branch_exists(&self, branch: &str) -> bool {
        let mut command = Command::new("git");
        command
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(&self.repository);
        run_bounded(command, Duration::from_secs(15))
            .map(|f| f.success)
            .unwrap_or(false)
    }

    /// Machine-derived state of `record`'s worktree.
    pub fn inspect(&self, record: &WorktreeRecord) -> Result<WorktreeInspection, WorktreeError> {
        let path = Path::new(&record.path);
        if !path.is_dir() {
            return Err(WorktreeError::NotFound(record.worktree_id.clone()));
        }
        let head = self.head(Some(path))?;
        let clean = self.is_clean(Some(path))?;
        let range = format!("{}..{head}", record.base_commit);
        let commits = non_empty_lines(&self.run(&["log", "--format=%H", &range], Some(path))?);
        let changed_files =
            non_empty_lines(&self.run(&["diff", "--name-only", &range], Some(path))?);
        let diff = self.run(&["diff", "--no-ext-diff", &range], Some(path))?;
        let result = WorkerResult {
            task_id: record.task_id.clone(),
            worker_session_id: String::new(),
            worktree_id: record.worktree_id.clone(),
            base_commit: record.base_commit.clone(),
            head_commit: head.clone(),
            commits,
            changed_files,
            clean,
            status: if clean && head != record.base_commit {
                "complete".into()
            } else {
                "incomplete".into()
            },
            summary: String::new(),
            tests: Vec::new(),
            error: String::new(),
        };
        let mut refreshed = record.clone();
        refreshed.head_commit = head;
        refreshed.dirty = !clean;
        refreshed.updated_at = now_secs();
        Ok(WorktreeInspection {
            record: refreshed,
            result,
            diff,
        })
    }

    /// Validate a worker's immutable base and current Git evidence.
    pub fn validate(&self, record: &WorktreeRecord) -> Result<WorkerResult, WorktreeError> {
        let repository = Path::new(&record.repository);
        let repository = repository
            .canonicalize()
            .unwrap_or_else(|_| repository.to_path_buf());
        if repository != self.repository {
            return Err(WorktreeError::Invalid(
                "worktree belongs to a different repository".into(),
            ));
        }
        self.validate_base(&record.base_commit)?;
        Ok(self.inspect(record)?.result)
    }

    /// Completion evidence for a worker; a dirty tree reports `dirty`.
    pub fn complete(
        &self,
        record: &WorktreeRecord,
        worker_session_id: &str,
    ) -> Result<WorkerResult, WorktreeError> {
        let mut result = self.inspect(record)?.result;
        result.worker_session_id = worker_session_id.to_string();
        if !result.clean {
            result.status = "dirty".into();
        }
        Ok(result)
    }

    /// Merge a worker branch into the coordinator; preserve it on conflict.
    pub fn integrate(
        &self,
        record: &WorktreeRecord,
        expected_branch: Option<&str>,
    ) -> Result<IntegrationResult, WorktreeError> {
        self.ensure_clean(None)?;
        if !Path::new(&record.path).is_dir() {
            return Err(WorktreeError::NotFound(record.worktree_id.clone()));
        }
        let current = self.branch(None)?;
        if let Some(expected) = expected_branch {
            if current != expected {
                return Err(WorktreeError::Invalid(format!(
                    "coordinator branch changed: expected {expected}, found {current}"
                )));
            }
        }
        let mut command = Command::new("git");
        command
            .args(["merge", "--no-edit", &record.branch])
            .current_dir(&self.repository);
        let finished = run_bounded(command, MERGE_TIMEOUT).map_err(|e| WorktreeError::Git {
            command: "merge".into(),
            output: format!("failed to start: {e}"),
        })?;
        if finished.success {
            let message = if finished.stdout.trim().is_empty() {
                finished.stderr.trim().to_string()
            } else {
                finished.stdout.trim().to_string()
            };
            return Ok(IntegrationResult {
                ok: true,
                repository: self.repository.display().to_string(),
                branch: record.branch.clone(),
                head_commit: self.head(None)?,
                conflict_paths: Vec::new(),
                message,
            });
        }
        let conflicts = self
            .git(
                &["diff", "--name-only", "--diff-filter=U"],
                None,
                false,
                GIT_TIMEOUT,
            )
            .map(|o| non_empty_lines(&o))
            .unwrap_or_default();
        // Abort only the coordinator merge; the worker worktree is untouched.
        let _ = self.git(&["merge", "--abort"], None, false, GIT_TIMEOUT);
        Ok(IntegrationResult {
            ok: false,
            repository: self.repository.display().to_string(),
            branch: record.branch.clone(),
            head_commit: String::new(),
            conflict_paths: conflicts,
            message: truncate(&format!("{}{}", finished.stdout, finished.stderr), 2000),
        })
    }

    /// Rebase a worker branch onto `onto`; preserve the worker on conflict.
    pub fn rebase(
        &self,
        record: &WorktreeRecord,
        onto: Option<&str>,
    ) -> Result<IntegrationResult, WorktreeError> {
        let path = Path::new(&record.path);
        self.ensure_clean(Some(path))?;
        let target = match onto {
            Some(target) => target.to_string(),
            None => self.branch(None)?,
        };
        let mut command = Command::new("git");
        command.args(["rebase", &target]).current_dir(path);
        let finished = run_bounded(command, MERGE_TIMEOUT).map_err(|e| WorktreeError::Git {
            command: "rebase".into(),
            output: format!("failed to start: {e}"),
        })?;
        if finished.success {
            return Ok(IntegrationResult {
                ok: true,
                repository: self.repository.display().to_string(),
                branch: record.branch.clone(),
                head_commit: self.head(Some(path))?,
                conflict_paths: Vec::new(),
                message: finished.stdout.trim().to_string(),
            });
        }
        let conflicts = self
            .git(
                &["diff", "--name-only", "--diff-filter=U"],
                Some(path),
                false,
                GIT_TIMEOUT,
            )
            .map(|o| non_empty_lines(&o))
            .unwrap_or_default();
        let _ = self.git(&["rebase", "--abort"], Some(path), false, GIT_TIMEOUT);
        Ok(IntegrationResult {
            ok: false,
            repository: self.repository.display().to_string(),
            branch: record.branch.clone(),
            head_commit: String::new(),
            conflict_paths: conflicts,
            message: truncate(&format!("{}{}", finished.stdout, finished.stderr), 2000),
        })
    }

    /// Remove an integrated, clean worktree (or any worktree with `force`).
    pub fn remove(
        &self,
        record: &WorktreeRecord,
        force: bool,
    ) -> Result<WorktreeRecord, WorktreeError> {
        let path = Path::new(&record.path);
        if !force {
            let inspection = self.inspect(record)?;
            if !inspection.result.clean {
                return Err(WorktreeError::Dirty(format!(
                    "refusing to remove dirty worker {}",
                    record.worktree_id
                )));
            }
            if !matches!(
                record.status,
                WorktreeStatus::Integrated | WorktreeStatus::Removed
            ) {
                return Err(WorktreeError::Invalid(
                    "worker may be removed only after successful integration".into(),
                ));
            }
        }
        if path.exists() {
            let path_text = path.display().to_string();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&path_text);
            self.run(&args, None)?;
        }
        let _ = self.git(&["branch", "-D", &record.branch], None, false, GIT_TIMEOUT);
        let mut removed = record.clone();
        removed.status = WorktreeStatus::Removed;
        removed.dirty = false;
        removed.updated_at = now_secs();
        Ok(removed)
    }

    /// Git's exact worktree view, for reconciliation after a crash.
    ///
    /// Every worktree other than the main one is reported as
    /// [`WorktreeStatus::Orphaned`] with no owner: until the coordinator
    /// matches it to a manifest record, nothing is in charge of it.
    pub fn recover(&self) -> Result<Vec<WorktreeRecord>, WorktreeError> {
        let raw = self.run(&["worktree", "list", "--porcelain"], None)?;
        let mut records = Vec::new();
        let mut current: Vec<(String, String)> = Vec::new();
        let mut lines: Vec<&str> = raw.lines().collect();
        lines.push("");
        for line in lines {
            if !line.is_empty() {
                let (key, value) = line.split_once(' ').unwrap_or((line, ""));
                current.push((key.to_string(), value.to_string()));
                continue;
            }
            let get = |key: &str| {
                current
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
            };
            if let Some(path) = get("worktree") {
                let path = PathBuf::from(&path);
                let resolved = path.canonicalize().unwrap_or(path);
                if resolved != self.repository {
                    let branch = get("branch")
                        .unwrap_or_default()
                        .trim_start_matches("refs/heads/")
                        .to_string();
                    let dirty = !self.is_clean(Some(&resolved)).unwrap_or(false);
                    let now = now_secs();
                    records.push(WorktreeRecord {
                        worktree_id: resolved
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        repository: self.repository.display().to_string(),
                        path: resolved.display().to_string(),
                        branch,
                        base_commit: String::new(),
                        owner_session_id: String::new(),
                        task_id: String::new(),
                        status: WorktreeStatus::Orphaned,
                        head_commit: get("HEAD").unwrap_or_default(),
                        dirty,
                        conflict_paths: Vec::new(),
                        created_at: now,
                        updated_at: now,
                        last_error: String::new(),
                        environment_receipts: Vec::new(),
                    });
                }
            }
            current.clear();
        }
        Ok(records)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A throwaway Git repository for tests, built with the system `git`.

    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Run `git` in `dir`, panicking with its output on failure.
    pub fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "xaft-test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "xaft-test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("git must be installed for worktree tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A repository with one commit on `main`, plus a separate worktree root.
    pub struct Repo {
        pub _dir: tempfile::TempDir,
        pub path: PathBuf,
        pub worktrees: PathBuf,
    }

    impl Repo {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("repo");
            std::fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "-q", "-b", "main"]);
            git(&path, &["config", "user.name", "xaft-test"]);
            git(&path, &["config", "user.email", "test@example.invalid"]);
            git(&path, &["config", "commit.gpgsign", "false"]);
            std::fs::write(path.join("README.md"), "base\n").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "-q", "-m", "base"]);
            let worktrees = dir.path().join("worktrees");
            Self {
                _dir: dir,
                path: path.canonicalize().unwrap(),
                worktrees,
            }
        }

        /// Write `file` in `worktree` and commit it there.
        pub fn commit_in(&self, worktree: &Path, file: &str, body: &str) {
            std::fs::write(worktree.join(file), body).unwrap();
            git(worktree, &["add", "."]);
            git(worktree, &["commit", "-q", "-m", &format!("edit {file}")]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{Repo, git};
    use super::*;

    fn manager(repo: &Repo) -> WorktreeManager {
        WorktreeManager::with_root(&repo.path, &repo.worktrees).unwrap()
    }

    #[test]
    fn a_non_repository_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = WorktreeManager::with_root(dir.path(), &dir.path().join("w")).unwrap_err();
        assert!(err.to_string().starts_with("not a Git repository:"));
    }

    #[test]
    fn slugs_match_agenthicc() {
        assert_eq!(slug("parent session/1", "x"), "parent-session-1");
        assert_eq!(slug("..", "fallback"), "fallback");
        assert_eq!(slug(&"a".repeat(60), "x").len(), 48);
    }

    #[test]
    fn create_branches_from_head_and_inspect_reports_git_evidence() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let record = manager
            .create("parent-1", "task-a", None, Some("wt-a"), None)
            .unwrap();
        assert_eq!(record.status, WorktreeStatus::Active);
        assert_eq!(record.branch, "xaft/parent-1/task-a-wt-a");
        assert!(Path::new(&record.path).is_dir());
        assert_eq!(record.base_commit, manager.head(None).unwrap());

        let before = manager.inspect(&record).unwrap();
        assert_eq!(before.result.status, "incomplete");
        assert!(before.result.commits.is_empty());

        repo.commit_in(Path::new(&record.path), "feature.txt", "work\n");
        let after = manager.inspect(&record).unwrap();
        assert_eq!(after.result.status, "complete");
        assert_eq!(after.result.commits.len(), 1);
        assert_eq!(after.result.changed_files, vec!["feature.txt"]);
        assert!(after.diff.contains("+work"));
    }

    #[test]
    fn create_refuses_a_dirty_coordinator() {
        let repo = Repo::new();
        std::fs::write(repo.path.join("untracked.txt"), "x").unwrap();
        let err = manager(&repo)
            .create("p", "t", None, Some("wt-d"), None)
            .unwrap_err();
        assert!(err.to_string().starts_with("Git worktree is dirty"));
        assert!(!repo.worktrees.join("wt-d").exists());
    }

    #[test]
    fn integrate_merges_a_clean_worker_branch() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let record = manager.create("p", "t", None, Some("wt-i"), None).unwrap();
        repo.commit_in(Path::new(&record.path), "feature.txt", "work\n");

        let result = manager.integrate(&record, Some("main")).unwrap();
        assert!(result.ok, "{result:?}");
        assert!(repo.path.join("feature.txt").is_file());
        assert_eq!(result.head_commit, manager.head(None).unwrap());
    }

    #[test]
    fn a_conflicting_merge_is_aborted_and_reports_the_paths() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let record = manager.create("p", "t", None, Some("wt-c"), None).unwrap();
        repo.commit_in(Path::new(&record.path), "README.md", "worker\n");
        repo.commit_in(&repo.path, "README.md", "coordinator\n");

        let result = manager.integrate(&record, Some("main")).unwrap();
        assert!(!result.ok);
        assert_eq!(result.conflict_paths, vec!["README.md"]);
        // The coordinator is left clean, the worker branch intact.
        assert!(manager.is_clean(None).unwrap());
        assert_eq!(
            std::fs::read_to_string(repo.path.join("README.md")).unwrap(),
            "coordinator\n"
        );
        assert!(manager.branch_exists(&record.branch));
    }

    #[test]
    fn integrate_refuses_a_changed_coordinator_branch() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let record = manager.create("p", "t", None, Some("wt-b"), None).unwrap();
        let err = manager.integrate(&record, Some("release")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "coordinator branch changed: expected release, found main"
        );
    }

    #[test]
    fn recover_reports_git_worktrees_as_orphans() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let record = manager.create("p", "t", None, Some("wt-r"), None).unwrap();
        let recovered = manager.recover().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].path, record.path);
        assert_eq!(recovered[0].branch, record.branch);
        assert_eq!(recovered[0].status, WorktreeStatus::Orphaned);
        assert_eq!(recovered[0].worktree_id, "wt-r");
    }

    #[test]
    fn remove_requires_integration_unless_forced() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let record = manager.create("p", "t", None, Some("wt-x"), None).unwrap();
        let err = manager.remove(&record, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("only after successful integration")
        );
        let removed = manager.remove(&record, true).unwrap();
        assert_eq!(removed.status, WorktreeStatus::Removed);
        assert!(!Path::new(&record.path).exists());
        assert!(!manager.branch_exists(&record.branch));
    }

    #[test]
    fn an_existing_path_is_readopted_only_with_a_matching_identity() {
        let repo = Repo::new();
        let manager = manager(&repo);
        let first = manager.create("p", "t", None, Some("wt-o"), None).unwrap();
        let again = manager.create("p", "t", None, Some("wt-o"), None).unwrap();
        assert_eq!(again.path, first.path);
        assert_eq!(again.status, WorktreeStatus::Active);
        let err = manager
            .create("p", "t", None, Some("wt-o"), Some("other-branch"))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not match its recovery identity")
        );
    }

    #[test]
    fn validate_base_refuses_a_non_object_id() {
        let repo = Repo::new();
        let err = manager(&repo).validate_base("main").unwrap_err();
        assert_eq!(err.to_string(), "base_commit must be a Git object id");
        let head = git(&repo.path, &["rev-parse", "HEAD"]);
        assert_eq!(manager(&repo).validate_base(&head[..10]).unwrap(), head);
    }
}
