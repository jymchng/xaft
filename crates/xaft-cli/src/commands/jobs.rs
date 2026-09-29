//! Handler for `xaft jobs` subcommands.
//!
//! Mirrors `agenthicc.cli.commands.background`'s `jobs_*` commands: the same
//! subcommand names, the same `--json` / `--all` / `--trash` flags, and the same
//! confirmation line format (`"<action>: <id> -> <status>"`).

use xaft_runtime::ExitCode;

use crate::args::{JobsSubcommand, OutputFormat};
use crate::error::XaftError;
use crate::jobs::{JobError, JobStatus, JobStore};

/// Default recoverable-trash retention, matching agenthicc's
/// `background.trash_retention_days` default of 7 days.
const TRASH_RETENTION_DAYS: u64 = 7;

/// Execute `xaft jobs <subcommand>`.
pub async fn handle_jobs(subcommand: &JobsSubcommand) -> Result<ExitCode, XaftError> {
    let mut store = JobStore::open_default().map_err(fail)?;
    match subcommand {
        JobsSubcommand::List(args) => list(&mut store, args.all, args.trash, args.format),
        JobsSubcommand::Status(args) => status(&mut store, &args.session_id, args.format),
        JobsSubcommand::Cancel { session_id } => {
            mutate(&mut store, session_id, "cancel", JobStatus::Cancelled)
        }
        JobsSubcommand::Resume { session_id } => {
            mutate(&mut store, session_id, "resume", JobStatus::Queued)
        }
        JobsSubcommand::Retry { session_id } => {
            mutate(&mut store, session_id, "retry", JobStatus::Retrying)
        }
        JobsSubcommand::Approve { session_id } => decide(&mut store, session_id, true),
        JobsSubcommand::Reject { session_id } => decide(&mut store, session_id, false),
        JobsSubcommand::Input { session_id, value } => input(&mut store, session_id, value),
        JobsSubcommand::Rename { session_id, title } => rename(&mut store, session_id, title),
        JobsSubcommand::Labels { session_id, labels } => {
            set_labels(&mut store, session_id, labels)
        }
        JobsSubcommand::Archive { session_id } => {
            mutate(&mut store, session_id, "archive", JobStatus::Archived)
        }
        JobsSubcommand::Delete { session_id } => delete(&mut store, session_id),
        JobsSubcommand::Restore { session_id } => restore(&mut store, session_id),
        JobsSubcommand::Purge => purge(&mut store),
    }
}

/// Surface a store failure as a CLI error.
///
/// `JobError` has no `XaftError` variant of its own; the store's only
/// unrecoverable failures are I/O, so those are reported as such.
fn fail(e: JobError) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

fn list(
    store: &mut JobStore,
    all: bool,
    trash: bool,
    format: OutputFormat,
) -> Result<ExitCode, XaftError> {
    let jobs = store.list(all, trash);
    if format == OutputFormat::Json {
        let payload: Vec<_> = jobs.iter().map(|j| j.public()).collect();
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(ExitCode::SUCCESS);
    }
    if jobs.is_empty() {
        println!("No background sessions.");
        return Ok(ExitCode::SUCCESS);
    }
    for job in &jobs {
        let id: String = job.session_id.chars().take(16).collect();
        let title: String = job.title.chars().take(48).collect();
        let flow = if job.workflow_name.is_empty() {
            "direct".to_string()
        } else {
            job.workflow_name.clone()
        };
        println!("{id}  {status:<16}  {title}  {flow}", status = job.status);
    }
    Ok(ExitCode::SUCCESS)
}

fn status(
    store: &mut JobStore,
    session_id: &str,
    format: OutputFormat,
) -> Result<ExitCode, XaftError> {
    // agenthicc reports a missing id as status "not_found" rather than failing,
    // so scripts can probe without special-casing the exit code.
    let payload = match store.get(session_id, true) {
        Ok(job) => job,
        Err(_) => {
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({ "session_id": session_id, "status": "not_found" })
                );
            } else {
                println!("Session: {session_id}");
                println!("State: not_found");
            }
            return Ok(ExitCode::SUCCESS);
        }
    };
    if format == OutputFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload.public()).unwrap_or_default()
        );
    } else {
        println!("Session: {}", payload.session_id);
        println!("State: {}", payload.status);
        println!("Title: {}", payload.title);
        println!("Activity: {}", payload.latest_activity);
        if let Some(err) = &payload.error {
            println!("Error: {err}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn mutate(
    store: &mut JobStore,
    session_id: &str,
    action: &str,
    target: JobStatus,
) -> Result<ExitCode, XaftError> {
    match store.transition(session_id, target) {
        Ok(job) => {
            println!("{action}: {} -> {}", job.session_id, job.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to {action} {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn decide(
    store: &mut JobStore,
    session_id: &str,
    approved: bool,
) -> Result<ExitCode, XaftError> {
    let action = if approved { "approve" } else { "reject" };
    match store.approve(session_id, approved) {
        Ok(job) => {
            println!("{action}: {} -> {}", job.session_id, job.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to {action} {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn input(
    store: &mut JobStore,
    session_id: &str,
    value: &str,
) -> Result<ExitCode, XaftError> {
    match store.provide_input(session_id, value) {
        Ok(job) => {
            println!("input: {} -> {}", job.session_id, job.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to provide input to {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn rename(
    store: &mut JobStore,
    session_id: &str,
    title: &str,
) -> Result<ExitCode, XaftError> {
    match store.rename(session_id, title) {
        Ok(job) => {
            println!("rename: {} -> {}", job.session_id, job.title);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to rename {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn set_labels(
    store: &mut JobStore,
    session_id: &str,
    raw: &str,
) -> Result<ExitCode, XaftError> {
    let values: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    match store.set_labels(session_id, values) {
        Ok(job) => {
            let shown = if job.labels.is_empty() {
                "none".to_string()
            } else {
                job.labels.join(", ")
            };
            println!("labels: {} -> {shown}", job.session_id);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to set labels on {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn delete(store: &mut JobStore, session_id: &str) -> Result<ExitCode, XaftError> {
    match store.delete(session_id) {
        Ok(job) => {
            println!("delete: {} -> {}", job.session_id, job.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to delete {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn restore(store: &mut JobStore, session_id: &str) -> Result<ExitCode, XaftError> {
    match store.restore(session_id) {
        Ok(job) => {
            println!("restore: {} -> {}", job.session_id, job.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("Unable to restore {session_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn purge(store: &mut JobStore) -> Result<ExitCode, XaftError> {
    let secs = (TRASH_RETENTION_DAYS * 24 * 60 * 60) as f64;
    match store.purge_expired_trash(secs) {
        Ok(removed) => {
            println!("purged: {}", removed.len());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => Err(fail(e)),
    }
}
