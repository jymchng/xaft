//! Handler for `xaft runs` subcommands.
//!
//! Mirrors `agenthicc.cli.commands.runs`: the same four subcommands
//! (`list`, `show`, `cancel`, `resume`), the same `--status` / `--json` flags,
//! and the same exit codes.
//!
//! The output formats are transcribed from agenthicc because scripts depend on
//! them:
//!
//! * `runs list` prints `run_id[:24]  status:<16>  workflow:<16>  goal[:64]`,
//!   or the literal `No goal runs.` when there are none;
//! * `runs show` prints pretty JSON **in both modes** — agenthicc's two
//!   branches are identical (`if json: ... else: ...` with the same body), so
//!   `--json` there is effectively decorative;
//! * `runs cancel` prints `cancel: <run_id> -> <status>` and
//!   `runs resume` prints `resume: <main_session_id> -> <status>`. Note the
//!   asymmetry: cancel reports the run, resume reports the *session*.
//!
//! Exit codes follow agenthicc: an unknown `--status` is 2, an unknown run id
//! is 1.

use xaft_runtime::ExitCode;

use crate::args::{OutputFormat, RunsSubcommand};
use crate::error::XaftError;
use crate::runs::{RunError, RunStore};

/// agenthicc's `runs list` prints this when the store is empty.
const NO_RUNS: &str = "No goal runs.";

/// Execute `xaft runs <subcommand>`.
pub async fn handle_runs(subcommand: &RunsSubcommand) -> Result<ExitCode, XaftError> {
    let mut store = RunStore::open_default().map_err(fail)?;
    match subcommand {
        RunsSubcommand::List(args) => list(&store, args.status.as_str(), args.format),
        RunsSubcommand::Show { run_id, json: _ } => show(&store, run_id),
        RunsSubcommand::Cancel { run_id } => cancel(&mut store, run_id),
        RunsSubcommand::Resume { run_id } => resume(&mut store, run_id),
    }
}

/// Surface a store failure as a CLI error.
fn fail(e: RunError) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

/// agenthicc exits 2 on a bad `--status` and 1 on a missing run; the store
/// error already carries the message, so only the code needs mapping.
fn code_for(e: &RunError) -> ExitCode {
    match e {
        RunError::InvalidStatus(_) => ExitCode::USAGE_ERROR,
        _ => ExitCode::TASK_FAILED,
    }
}

fn list(store: &RunStore, status: &str, format: OutputFormat) -> Result<ExitCode, XaftError> {
    let runs = match store.list_by_status(Some(status)) {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!("{}", e);
            return Ok(code_for(&e));
        }
    };

    if matches!(format, OutputFormat::Json) {
        let payload: Vec<_> = runs.iter().map(|r| r.to_projection()).collect();
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
        return Ok(ExitCode::SUCCESS);
    }

    if runs.is_empty() {
        println!("{NO_RUNS}");
        return Ok(ExitCode::SUCCESS);
    }
    for run in runs {
        println!(
            "{}  {}  {}  {}",
            truncate(&run.run_id, 24),
            pad(&run.status.to_string(), 16),
            truncate(&run.workflow_name, 16),
            truncate(&run.goal, 64)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `runs show` prints pretty JSON whether or not `--json` was passed —
/// agenthicc's two branches are byte-identical.
fn show(store: &RunStore, run_id: &str) -> Result<ExitCode, XaftError> {
    let run = match store.get(run_id) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("{e}");
            return Ok(ExitCode::TASK_FAILED);
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&run.to_projection()).map_err(io)?
    );
    Ok(ExitCode::SUCCESS)
}

fn cancel(store: &mut RunStore, run_id: &str) -> Result<ExitCode, XaftError> {
    match store.cancel(run_id) {
        Ok(run) => {
            println!("cancel: {} -> {}", run.run_id, run.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("Unable to cancel {run_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn resume(store: &mut RunStore, run_id: &str) -> Result<ExitCode, XaftError> {
    match store.resume(run_id) {
        Ok(run) => {
            // agenthicc reports the main *session* here, not the run.
            println!("resume: {} -> {}", run.main_session_id, run.status);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("Unable to resume {run_id}: {e}");
            Ok(ExitCode::TASK_FAILED)
        }
    }
}

fn io(e: serde_json::Error) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

/// Python-style `[0:n]` truncation, matching agenthicc's `str(...)[:n]`.
fn truncate(value: &str, limit: usize) -> &str {
    match value.char_indices().nth(limit) {
        Some((idx, _)) => &value[..idx],
        None => value,
    }
}

/// Left-justify to `width`, as Python's `:<16` does. Never truncates, so a
/// longer value simply pushes the rest of the line right.
fn pad(value: &str, width: usize) -> String {
    if value.chars().count() >= width {
        value.to_string()
    } else {
        format!("{value:<width$}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries_not_bytes() {
        // A 3-byte-per-char string: a byte slice would panic here.
        let s = "日本語日本語";
        assert_eq!(truncate(s, 3), "日本語");
        assert_eq!(truncate(s, 99), s);
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn pad_left_justifies_without_truncating() {
        // 9 chars, padded with 7 spaces to the field width of 16.
        assert_eq!(pad("completed", 16), "completed       ");
        // 15 chars, so exactly one space of padding -- it is NOT longer than
        // the field. Getting this wrong is an easy off-by-one.
        assert_eq!(pad("needs_attention", 16), "needs_attention ");
        // Longer than the field: kept whole, exactly like Python's `:<16`.
        assert_eq!(
            pad("a_very_long_workflow_name", 16),
            "a_very_long_workflow_name"
        );
        // Exactly the field width: no padding at all.
        assert_eq!(pad("0123456789abcdef", 16), "0123456789abcdef");
    }

    #[test]
    fn invalid_status_maps_to_usage_and_others_to_failure() {
        assert_eq!(
            code_for(&RunError::InvalidStatus("x".into())),
            ExitCode::USAGE_ERROR
        );
        assert_eq!(
            code_for(&RunError::NotFound("x".into())),
            ExitCode::TASK_FAILED
        );
        assert_eq!(
            code_for(&RunError::NoMainSession("x".into())),
            ExitCode::TASK_FAILED
        );
    }
}
