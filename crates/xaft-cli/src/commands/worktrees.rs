//! Handler for `xaft worktrees` (parity matrix rows 50–53).
//!
//! Port of the four inspection and recovery commands in
//! `agenthicc.cli.commands.worktrees`, over the orchestration layer in
//! [`crate::worktrees`]:
//!
//! | command | output |
//! | --- | --- |
//! | `worktrees list [--json]` | `<id16>  <status:12>  <n> task(s)  parent=<session>`, or the manifests as JSON |
//! | `worktrees show ID [--json]` | the manifest as JSON (both forms, as in agenthicc) |
//! | `worktrees recover ID [--repository DIR]` | the reconciled manifest as JSON |
//! | `worktrees integrate ID TASK [--repository DIR]` | the integration result as JSON |
//!
//! JSON is `indent=2, sort_keys=True` — see [`crate::worktrees::store`] for how
//! that is reproduced byte for byte. Failures print agenthicc's
//! `Unable to recover orchestration: …` / `Unable to integrate task: …` /
//! `Orchestration not found: …` and exit 1, as agenthicc's `SystemExit(1)`
//! does. A *conflicting* integration is not a failure: it prints the result
//! with `"ok": false` and the conflicting paths, and exits 0, exactly like
//! agenthicc.
//!
//! One divergence: `worktrees show` with a malformed id or an unreadable
//! manifest prints `Unable to show orchestration: …` and exits 1, where
//! agenthicc lets the `ValueError` escape as a traceback.

use std::fmt::Write as _;
use std::path::Path;

use xaft_runtime::ExitCode;

use crate::args::WorktreesSubcommand;
use crate::error::XaftError;
use crate::worktrees::error::{pad_status, short_id};
use crate::worktrees::store::render;
use crate::worktrees::{ManifestStore, ParallelCoordinator, WorktreeError, WorktreeManager};

/// The empty-state line of `worktrees list`.
pub const NO_ORCHESTRATIONS: &str = "No parallel orchestrations.";

/// Execute `xaft worktrees <subcommand>` against the default stores.
pub async fn handle_worktrees(subcommand: &WorktreesSubcommand) -> Result<ExitCode, XaftError> {
    let store = ManifestStore::open_default()
        .map_err(|e| XaftError::Io(std::io::Error::other(e.to_string())))?;
    let mut out = String::new();
    let code = render_worktrees(subcommand, &store, None, &mut out);
    print!("{out}");
    Ok(code)
}

/// Run `xaft worktrees` against `store`, writing every line into `out`.
///
/// `worktree_root` overrides where a manager places worktrees (default
/// `$XAFT_HOME/worktrees/<repo>`); tests use it to stay inside a temp dir.
pub fn render_worktrees(
    subcommand: &WorktreesSubcommand,
    store: &ManifestStore,
    worktree_root: Option<&Path>,
    out: &mut String,
) -> ExitCode {
    match subcommand {
        WorktreesSubcommand::List { json } => list(store, *json, out),
        WorktreesSubcommand::Show {
            orchestration_id, ..
        } => show(store, orchestration_id, out),
        WorktreesSubcommand::Recover {
            orchestration_id,
            repository,
        } => {
            let outcome = coordinator_for(store, orchestration_id, repository, worktree_root)
                .and_then(|c| c.recover(orchestration_id))
                .and_then(|manifest| render(&manifest));
            emit(outcome, "Unable to recover orchestration", out)
        }
        WorktreesSubcommand::Integrate {
            orchestration_id,
            task_id,
            repository,
        } => {
            let outcome = coordinator_for(store, orchestration_id, repository, worktree_root)
                .and_then(|c| c.integrate_task(orchestration_id, task_id))
                .and_then(|result| render(&result));
            emit(outcome, "Unable to integrate task", out)
        }
    }
}

fn list(store: &ManifestStore, json: bool, out: &mut String) -> ExitCode {
    let manifests = store.list();
    if json {
        return match render(&manifests) {
            Ok(text) => {
                let _ = writeln!(out, "{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                let _ = writeln!(out, "Unable to list orchestrations: {e}");
                ExitCode::TASK_FAILED
            }
        };
    }
    if manifests.is_empty() {
        let _ = writeln!(out, "{NO_ORCHESTRATIONS}");
        return ExitCode::SUCCESS;
    }
    for manifest in &manifests {
        let _ = writeln!(
            out,
            "{}  {}  {} task(s)  parent={}",
            short_id(&manifest.orchestration_id),
            pad_status(manifest.status.as_str(), 12),
            manifest.tasks.len(),
            manifest.parent_session_id
        );
    }
    ExitCode::SUCCESS
}

fn show(store: &ManifestStore, orchestration_id: &str, out: &mut String) -> ExitCode {
    match store.get(orchestration_id).and_then(|m| render(&m)) {
        Ok(text) => {
            let _ = writeln!(out, "{text}");
            ExitCode::SUCCESS
        }
        Err(WorktreeError::NotFound(id)) => {
            let _ = writeln!(out, "Orchestration not found: {id}");
            ExitCode::TASK_FAILED
        }
        Err(e) => {
            let _ = writeln!(out, "Unable to show orchestration: {e}");
            ExitCode::TASK_FAILED
        }
    }
}

/// Load the manifest first (so a missing one is reported before the
/// repository is even resolved, as in agenthicc), then build a coordinator
/// acting for the manifest's parent session.
fn coordinator_for(
    store: &ManifestStore,
    orchestration_id: &str,
    repository: &Path,
    worktree_root: Option<&Path>,
) -> Result<ParallelCoordinator, WorktreeError> {
    let manifest = store.get(orchestration_id)?;
    let manager = match worktree_root {
        Some(root) => WorktreeManager::with_root(repository, root)?,
        None => WorktreeManager::new(repository)?,
    };
    Ok(
        ParallelCoordinator::new(manager, store.clone(), manifest.parent_session_id)
            .with_run_id(manifest.run_id),
    )
}

fn emit(outcome: Result<String, WorktreeError>, failure: &str, out: &mut String) -> ExitCode {
    match outcome {
        Ok(text) => {
            let _ = writeln!(out, "{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(out, "{failure}: {e}");
            ExitCode::TASK_FAILED
        }
    }
}
