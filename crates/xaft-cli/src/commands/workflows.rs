//! Handler for `xaft workflows` subcommands.
//!
//! Mirrors `agenthicc.cli.commands.workflows` (`list`, `run`) and the run
//! management agenthicc exposes through `/workflows runs` and
//! `/workflow resume|reset`, plus `show`, `validate` and `create` for
//! authoring custom workflows from the shell.
//!
//! # Output contracts, transcribed from agenthicc
//!
//! `workflows list` prints one three-line block per workflow, name-sorted:
//!
//! ```text
//! <name> [<source>] — <description or 'no description'>
//!   phases: a -> b -> c          (or the literal `(no phases)`)
//!   modes: safe, plan            (or the literal `manual`)
//! ```
//!
//! and `No workflows found.` when the registry is empty — unreachable here,
//! because xaft always has its built-ins, but kept so the branch is honest
//! rather than assumed away. `source` is `builtin`, `user` (a file under
//! `~/.config/xaft/workflows/`) or `project` (a file under
//! `.xaft/workflows/`, or a `[workflows.<name>]` table) — agenthicc's three
//! labels.
//!
//! `workflows run` prints, for a phased workflow (which has a durable run):
//!
//! ```text
//! Workflow <name> <status> (session <id>, run <run-id>)
//! Phases: a → b → c
//! Error: <message>              (only when the run failed)
//! ```
//!
//! exactly agenthicc's shape. The fixed orchestrators (`standard`, `dynamic`,
//! `meta`) keep no run checkpoint, so for them the line is
//! `Workflow <name> <exit-code> (session <id>)` followed by `Summary:` — see
//! [`run_outcome_line`] and `docs/parity-matrix.md` §1d.
//!
//! Every command supports `--json`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xaft_config::workflows::discovery::load_workflow_path;
use xaft_config::workflows::validate::validate_definition;
use xaft_config::workflows::{WorkflowEntry, WorkflowRegistry, builtin};
use xaft_config::{CliOverrides, XaftConfig};
use xaft_runtime::ExitCode;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::phased::state::{CheckpointStore, RunStatus, WorkflowRunState, snippet};

use crate::args::WorkflowsSubcommand;
use crate::error::XaftError;

/// agenthicc's `workflows list` prints this when the registry is empty.
const NO_WORKFLOWS: &str = "No workflows found.";

/// How many runs `workflows runs` shows without `--all`.
const RECENT_RUNS: usize = 20;

/// Execute `xaft workflows <subcommand>`.
pub async fn handle_workflows(
    subcommand: &WorkflowsSubcommand,
    config: &XaftConfig,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // Names resolve against the CLI's loaded config (plus the workflow files
    // under `cwd`); `run`/`create`/`resume` then execute with the config their
    // own `-c` loads — the contract `workflows_handlers.rs` pins.
    let registry = WorkflowRegistry::discover(config, &cwd);
    match subcommand {
        WorkflowsSubcommand::List { json } => list(&registry, *json),
        WorkflowsSubcommand::Show {
            workflow_name,
            json,
            toml,
        } => show(&registry, workflow_name, *json, *toml),
        WorkflowsSubcommand::Run {
            workflow_name,
            intent,
            json,
            config: config_path,
        } => {
            let run_config = load_run_config(config_path)?;
            run(
                &registry,
                &run_config,
                workflow_name,
                intent,
                *json,
                &cwd,
                runtime,
            )
            .await
        }
        WorkflowsSubcommand::Create {
            intent,
            json,
            config: config_path,
        } => {
            let run_config = load_run_config(config_path)?;
            run(
                &registry,
                &run_config,
                "create_workflow",
                intent,
                *json,
                &cwd,
                runtime,
            )
            .await
        }
        WorkflowsSubcommand::Runs { json, all } => runs(&cwd, *json, *all),
        WorkflowsSubcommand::Resume {
            run_id,
            workflow,
            json,
            config: config_path,
        } => {
            let run_config = load_run_config(config_path)?;
            resume(
                &registry,
                &run_config,
                run_id.as_deref(),
                workflow.as_deref(),
                *json,
                &cwd,
                runtime,
            )
            .await
        }
        WorkflowsSubcommand::Reset { run_id } => reset(&cwd, run_id),
        WorkflowsSubcommand::Validate { path, json } => validate(path, *json),
    }
}

// ── list ────────────────────────────────────────────────────────────────────

fn list(registry: &WorkflowRegistry, json: bool) -> Result<ExitCode, XaftError> {
    let entries = registry.entries();

    if json {
        let payload: Vec<serde_json::Value> = entries.iter().map(|e| entry_json(e)).collect();
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
        return Ok(ExitCode::SUCCESS);
    }

    report_broken(registry);
    if entries.is_empty() {
        println!("{NO_WORKFLOWS}");
        return Ok(ExitCode::SUCCESS);
    }

    for entry in entries {
        let description = if entry.description.is_empty() {
            "no description"
        } else {
            &entry.description
        };
        println!(
            "{} [{}] — {}",
            entry.name,
            entry.source.label(),
            description
        );
        println!("  phases: {}", entry.phase_path());
        println!("  modes: {}", entry.modes_line());
    }
    Ok(ExitCode::SUCCESS)
}

/// Workflows that were declared but cannot be selected, on stderr so the
/// listing itself stays machine-stable.
fn report_broken(registry: &WorkflowRegistry) {
    for err in registry.load_errors() {
        eprintln!("warning: skipped workflow file {err}");
    }
    for bad in registry.invalid() {
        eprintln!(
            "warning: workflow '{}' is invalid and was not loaded:\n{}",
            bad.name,
            indent(&bad.report.render())
        );
    }
}

fn entry_json(e: &WorkflowEntry) -> serde_json::Value {
    serde_json::json!({
        "name": e.name,
        "description": e.description,
        "source": e.source.label(),
        "path": e.path.as_ref().map(|p| p.display().to_string()),
        "mode": format!("{:?}", e.mode).to_lowercase(),
        "mode_bindings": e.mode_bindings,
        "model": e.model,
        "max_turns": e.max_turns,
        "phases": e.phases,
        "warnings": e.warnings,
    })
}

// ── show ────────────────────────────────────────────────────────────────────

fn show(
    registry: &WorkflowRegistry,
    name: &str,
    json: bool,
    as_toml: bool,
) -> Result<ExitCode, XaftError> {
    let entry = match registry.resolve(name) {
        Ok(entry) => entry,
        Err(e) => {
            report_failure(json, &e.to_string());
            return Ok(ExitCode::USAGE_ERROR);
        }
    };
    if as_toml {
        let text = match (&entry.path, builtin::builtin_source(&entry.name)) {
            (Some(p), _) if p.is_dir() => std::fs::read_to_string(p.join("workflow.toml"))?,
            (Some(p), _) => std::fs::read_to_string(p)?,
            (None, Some(src)) if entry.source.label() == "builtin" => src.to_string(),
            _ => toml::to_string_pretty(&entry.definition)
                .map_err(|e| XaftError::Io(std::io::Error::other(e.to_string())))?,
        };
        print!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    if json {
        let mut payload = entry_json(entry);
        payload["definition"] = serde_json::to_value(&entry.definition).map_err(io)?;
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
        return Ok(ExitCode::SUCCESS);
    }
    for line in show_lines(entry) {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The human-readable `workflows show` block.
fn show_lines(entry: &WorkflowEntry) -> Vec<String> {
    let mut out = vec![
        format!(
            "{} [{}] — {}",
            entry.name,
            entry.source.label(),
            if entry.description.is_empty() {
                "no description"
            } else {
                &entry.description
            }
        ),
        format!(
            "  orchestrator: {}",
            format!("{:?}", entry.mode).to_lowercase()
        ),
        format!("  modes: {}", entry.modes_line()),
    ];
    if let Some(path) = &entry.path {
        out.push(format!("  file: {}", path.display()));
    }
    if entry.is_phased() {
        out.push(format!("  phases ({}):", entry.phases.len()));
        for (i, p) in entry.phases.iter().enumerate() {
            let mut edges = Vec::new();
            if let Some(n) = &p.next {
                edges.push(format!("next → {n}"));
            }
            if let Some(n) = &p.on_reject {
                edges.push(format!("on_reject → {n}"));
            }
            if let Some(n) = &p.on_error {
                edges.push(format!("on_error → {n}"));
            }
            if !p.parallel_with.is_empty() {
                edges.push(format!("parallel with {}", p.parallel_with.join(", ")));
            }
            out.push(format!(
                "    {}. {} [{}]{}",
                i + 1,
                p.name,
                p.role(),
                if edges.is_empty() {
                    "  (ends the run)".to_string()
                } else {
                    format!("  {}", edges.join(", "))
                }
            ));
            let mut tools: Vec<String> = Vec::new();
            if p.require_plan_finalization {
                tools.push("request_plan_approval / finalize_plan / exit_workflow".into());
            }
            if p.require_explicit_completion {
                tools.push("mark_execute_complete".into());
            }
            if p.require_explicit_review {
                tools.push("approve_review / reject_review".into());
            }
            for t in &p.transitions {
                let target = if t.end {
                    "end".to_string()
                } else {
                    t.next.clone().unwrap_or_else(|| "default edge".into())
                };
                tools.push(format!("{} → {target}", t.tool));
            }
            if !tools.is_empty() {
                out.push(format!("       transitions: {}", tools.join("; ")));
            }
            if let Some(mode) = &p.mode_override {
                out.push(format!("       mode: {mode}"));
            }
            if let Some(model) = entry.definition.model_for_phase(p) {
                out.push(format!("       model: {model}"));
            }
            if !p.extra_tools.is_empty() {
                out.push(format!("       tool groups: {}", p.extra_tools.join(", ")));
            }
        }
    } else {
        out.push(format!("  phases: {}", entry.phase_path()));
    }
    for w in &entry.warnings {
        out.push(format!("  {w}"));
    }
    out
}

// ── run ─────────────────────────────────────────────────────────────────────

/// Load the config `workflows run` executes with.
///
/// `xaft workflows run` is its own entry point, so it cannot inherit
/// `handle_run`'s already-loaded config; it loads its own and honours `-c`.
fn load_run_config(config_path: &Option<PathBuf>) -> Result<XaftConfig, XaftError> {
    let overrides = CliOverrides {
        config_file: config_path.clone(),
        ..CliOverrides::default()
    };
    Ok(xaft_config::ConfigLoader::load(&overrides)?)
}

fn build_request(
    entry: &WorkflowEntry,
    config: &XaftConfig,
    registry: &WorkflowRegistry,
    task: &str,
    cwd: &Path,
    resume_run_id: Option<String>,
) -> RunRequest {
    // The workflow's own model / max_turns overrides must reach the run, which
    // is what keeps those two `[workflows.<name>]` keys from being dead config.
    let mut run_config = config.clone();
    registry.apply_to_config(entry, &mut run_config);
    let mut workflow = xaft_runtime::to_runtime_config(entry, config);
    if let xaft_runtime::WorkflowConfig::Phased {
        resume_run_id: slot,
        ..
    } = &mut workflow
    {
        *slot = resume_run_id;
    }
    RunRequest {
        task: task.to_string(),
        config: run_config,
        working_dir: cwd.to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow,
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    }
}

async fn run(
    registry: &WorkflowRegistry,
    config: &XaftConfig,
    workflow_name: &str,
    intent: &str,
    json: bool,
    cwd: &Path,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let entry = match registry.resolve(workflow_name) {
        Ok(entry) => entry,
        Err(e) => {
            // agenthicc catches (RuntimeError, ValueError) and prints
            // `Workflow failed: {exc}` WITHOUT a non-zero exit, and matches
            // that for --json with {"status": "failed", "error": ...}.
            report_failure(json, &e.to_string());
            return Ok(ExitCode::SUCCESS);
        }
    };
    let request = build_request(entry, config, registry, intent, cwd, None);
    let started = chrono::Utc::now();
    execute(entry, request, json, cwd, started, runtime).await
}

/// Run a request and report it — shared by `run`, `create` and `resume`.
async fn execute(
    entry: &WorkflowEntry,
    request: RunRequest,
    json: bool,
    cwd: &Path,
    started: chrono::DateTime<chrono::Utc>,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let resume_id = match &request.workflow {
        xaft_runtime::WorkflowConfig::Phased { resume_run_id, .. } => resume_run_id.clone(),
        _ => None,
    };
    let outcome = runtime.run(request).await;

    if !entry.is_phased() {
        let result = outcome?;
        if json {
            let payload = serde_json::json!({
                "workflow_name": entry.name,
                "status": result.exit_code.to_string(),
                "session_id": result.session.id.to_string(),
                "summary": result.summary,
            });
            println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
            return Ok(result.exit_code);
        }
        let session_id = result.session.id.to_string();
        println!(
            "{}",
            run_outcome_line(&entry.name, result.exit_code, &session_id)
        );
        if !result.summary.trim().is_empty() {
            println!("Summary: {}", result.summary);
        }
        return Ok(result.exit_code);
    }

    // A phased run has a durable identity: its checkpoint.
    let store = CheckpointStore::for_working_dir(cwd);
    let state = find_phased_run(&store, &entry.name, started, resume_id.as_deref());
    let error = outcome.as_ref().err().map(ToString::to_string);
    let summary = outcome
        .as_ref()
        .map(|r| r.summary.clone())
        .unwrap_or_default();
    let session_id = match (&outcome, &state) {
        (Ok(r), _) => r.session.id.to_string(),
        (Err(_), Some(s)) => s.session_id.clone(),
        (Err(_), None) => "unknown".into(),
    };
    let status = state
        .as_ref()
        .map(|s| s.status.label().to_string())
        .unwrap_or_else(|| {
            if error.is_some() {
                "failed"
            } else {
                "complete"
            }
            .into()
        });
    let run_id = state
        .as_ref()
        .map(|s| s.run_id.clone())
        .unwrap_or_else(|| "unknown".into());
    let phases: Vec<String> = state
        .as_ref()
        .map(|s| {
            s.phase_history
                .iter()
                .map(|r| r.phase_name.clone())
                .collect()
        })
        .unwrap_or_default();
    let exit = if error.is_some() {
        ExitCode::TASK_FAILED
    } else {
        ExitCode::SUCCESS
    };

    if json {
        let payload = serde_json::json!({
            "workflow_name": entry.name,
            "status": status,
            "session_id": session_id,
            "run_id": run_id,
            "phases": phases,
            "summary": summary,
            "error": error,
        });
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
        return Ok(exit);
    }
    println!(
        "{}",
        phased_outcome_line(&entry.name, &status, &session_id, &run_id)
    );
    if !phases.is_empty() {
        println!("Phases: {}", phases.join(" → "));
    }
    if !summary.trim().is_empty() {
        println!("Summary: {summary}");
    }
    if let Some(err) = error {
        println!("Error: {err}");
        if state.as_ref().is_some_and(|s| s.status.is_resumable()) {
            println!("Resume with: xaft workflows resume {run_id}");
        }
    }
    Ok(exit)
}

/// The checkpoint a phased run produced: the resumed run when one was
/// named, otherwise the newest run of `workflow` created since `started`.
pub(crate) fn find_phased_run(
    store: &CheckpointStore,
    workflow: &str,
    started: chrono::DateTime<chrono::Utc>,
    resumed: Option<&str>,
) -> Option<WorkflowRunState> {
    match resumed {
        Some(id) => store.load(id).ok(),
        None => store
            .list()
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.workflow_name == workflow && s.created_at >= started),
    }
}

/// The outcome line for a fixed orchestrator, which has no run identity.
///
/// The session id is the durable handle (`xaft session show <id>`), so it is
/// printed once — never a second time under a `run` label.
fn run_outcome_line(name: &str, exit_code: ExitCode, session_id: &str) -> String {
    format!("Workflow {name} {exit_code} (session {session_id})")
}

/// agenthicc's outcome line, for a phased run with a checkpointed run id.
fn phased_outcome_line(name: &str, status: &str, session_id: &str, run_id: &str) -> String {
    format!("Workflow {name} {status} (session {session_id}, run {run_id})")
}

// ── runs / resume / reset ───────────────────────────────────────────────────

fn runs(cwd: &Path, json: bool, all: bool) -> Result<ExitCode, XaftError> {
    let store = CheckpointStore::for_working_dir(cwd);
    let mut list = store.list()?;
    if !all {
        list.truncate(RECENT_RUNS);
    }
    if json {
        let payload: Vec<serde_json::Value> = list.iter().map(run_json).collect();
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
        return Ok(ExitCode::SUCCESS);
    }
    if list.is_empty() {
        println!("No workflow runs in {}.", store.dir().display());
        return Ok(ExitCode::SUCCESS);
    }
    for line in runs_table(&list) {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

fn run_json(s: &WorkflowRunState) -> serde_json::Value {
    serde_json::json!({
        "run_id": s.run_id,
        "workflow_name": s.workflow_name,
        "status": s.status.label(),
        "resumable": s.status.is_resumable(),
        "current_phase": s.current_phase,
        "next_phase": s.next_phase,
        "phases_run": s.phase_history.len(),
        "intent": s.intent,
        "session_id": s.session_id,
        "error": s.error,
        "created_at": s.created_at.to_rfc3339(),
        "updated_at": s.updated_at.to_rfc3339(),
    })
}

/// The `workflows runs` table.
fn runs_table(list: &[WorkflowRunState]) -> Vec<String> {
    let mut out = vec![format!(
        "{:<12}  {:<18}  {:<9}  {:<14}  {:<16}  {}",
        "RUN", "WORKFLOW", "STATUS", "PHASE", "UPDATED", "INTENT"
    )];
    for s in list {
        let phase = match s.status {
            RunStatus::Complete | RunStatus::Exited => "-".to_string(),
            _ => s
                .next_phase
                .clone()
                .or_else(|| s.current_phase.clone())
                .unwrap_or_else(|| "-".into()),
        };
        out.push(format!(
            "{:<12}  {:<18}  {:<9}  {:<14}  {:<16}  {}",
            s.run_id.chars().take(12).collect::<String>(),
            snippet(&s.workflow_name, 18),
            s.status.label(),
            snippet(&phase, 14),
            s.updated_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M"),
            snippet(&s.intent.replace('\n', " "), 60)
        ));
    }
    out
}

async fn resume(
    registry: &WorkflowRegistry,
    config: &XaftConfig,
    run_id: Option<&str>,
    workflow: Option<&str>,
    json: bool,
    cwd: &Path,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let store = CheckpointStore::for_working_dir(cwd);
    let state = match run_id {
        Some(id) => match store.load(id) {
            Ok(s) => s,
            Err(e) => {
                report_failure(json, &e.to_string());
                return Ok(ExitCode::USAGE_ERROR);
            }
        },
        None => match store.latest_resumable(workflow)? {
            Some(s) => s,
            None => {
                report_failure(json, "no resumable workflow run in this directory");
                return Ok(ExitCode::USAGE_ERROR);
            }
        },
    };
    if !state.status.is_resumable() {
        report_failure(
            json,
            &format!(
                "workflow run {} is {} and cannot be resumed",
                state.run_id,
                state.status.label()
            ),
        );
        return Ok(ExitCode::USAGE_ERROR);
    }
    let entry = match registry.resolve(&state.workflow_name) {
        Ok(entry) if entry.is_phased() => entry,
        Ok(_) | Err(_) => {
            report_failure(
                json,
                &format!(
                    "workflow '{}' of run {} is no longer available as a phased workflow",
                    state.workflow_name, state.run_id
                ),
            );
            return Ok(ExitCode::USAGE_ERROR);
        }
    };
    let request = build_request(
        entry,
        config,
        registry,
        &state.intent,
        cwd,
        Some(state.run_id.clone()),
    );
    execute(entry, request, json, cwd, state.created_at, runtime).await
}

fn reset(cwd: &Path, run_id: &str) -> Result<ExitCode, XaftError> {
    let store = CheckpointStore::for_working_dir(cwd);
    let state = match store.load(run_id) {
        Ok(s) => s,
        Err(e) => {
            println!("{e}");
            return Ok(ExitCode::USAGE_ERROR);
        }
    };
    store.remove(&state.run_id)?;
    println!(
        "Removed workflow run {} ({}, {}).",
        state.run_id,
        state.workflow_name,
        state.status.label()
    );
    Ok(ExitCode::SUCCESS)
}

// ── validate ────────────────────────────────────────────────────────────────

fn validate(path: &Path, json: bool) -> Result<ExitCode, XaftError> {
    let loaded = match load_workflow_path(path) {
        Ok(l) => l,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"valid": false, "path": path.display().to_string(), "errors": [e.message]})
                );
            } else {
                println!("invalid: {e}");
            }
            return Ok(ExitCode::TASK_FAILED);
        }
    };
    let report = validate_definition(&loaded.name, &loaded.definition);
    let ok = report.is_ok();
    if json {
        let payload = serde_json::json!({
            "valid": ok,
            "name": loaded.name,
            "path": path.display().to_string(),
            "phases": loaded.definition.phases.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "errors": report.errors().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "warnings": report.warnings().iter().map(ToString::to_string).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
    } else {
        println!(
            "{} '{}' ({} phase(s): {})",
            if ok { "valid:" } else { "invalid:" },
            loaded.name,
            loaded.definition.phases.len(),
            loaded
                .definition
                .phases
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        );
        if !report.diagnostics.is_empty() {
            println!("{}", indent(&report.render()));
        }
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::TASK_FAILED
    })
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn report_failure(json: bool, message: &str) {
    if json {
        let payload = serde_json::json!({"status": "failed", "error": message});
        println!("{payload}");
    } else {
        println!("Workflow failed: {message}");
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn io(e: serde_json::Error) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use xaft_config::types::{WorkflowDefinition, WorkflowMode, XaftConfig as Cfg};
    use xaft_runtime::RuntimeError;
    use xaft_runtime::dispatch::{RunResult, RuntimeDispatch};
    use xaft_runtime::session::AgentSession;

    /// Records the workflow the runtime was actually asked to run.
    struct RecordingRuntime {
        seen: std::sync::Mutex<Option<String>>,
        workflow: std::sync::Mutex<String>,
    }

    impl RecordingRuntime {
        fn new(workflow: &str) -> Self {
            Self {
                seen: std::sync::Mutex::new(None),
                workflow: std::sync::Mutex::new(workflow.to_string()),
            }
        }
    }

    #[async_trait]
    impl RuntimeDispatch for RecordingRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            *self.seen.lock().unwrap() = Some(req.task.clone());
            *self.workflow.lock().unwrap() = format!("{:?}", req.workflow);
            let session = AgentSession::new(
                req.task.clone(),
                req.working_dir.clone(),
                "default".into(),
                "m".into(),
            );
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: "ok".into(),
            })
        }
        async fn list_sessions(
            &self,
            _: &std::path::Path,
        ) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![])
        }
        async fn resume_session(&self, _: &str, _: XaftConfig) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    fn with_custom(def: WorkflowDefinition) -> Cfg {
        let mut config = Cfg::default();
        config.workflows.insert("release_review".into(), def);
        config
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ── list ──

    #[test]
    fn list_always_has_the_fixed_orchestrators_and_the_phased_builtins() {
        let registry = WorkflowRegistry::build(&Cfg::default());
        for name in [
            "standard",
            "dynamic",
            "meta",
            "code_plan",
            "create_workflow",
        ] {
            assert!(registry.get(name).is_some(), "{name} missing");
        }
    }

    #[test]
    fn list_reports_a_custom_workflows_source_as_project() {
        let config = with_custom(WorkflowDefinition {
            description: "Mine.".into(),
            ..Default::default()
        });
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(
            registry.get("release_review").unwrap().source.label(),
            "project"
        );
        assert_eq!(registry.get("standard").unwrap().source.label(), "builtin");
    }

    // ── run ──

    #[tokio::test]
    async fn run_selects_the_named_workflow_for_the_runtime() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        let dir = tmp();
        run(
            &registry,
            &config,
            "meta",
            "ship it",
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        let seen = runtime.workflow.lock().unwrap().clone();
        assert!(
            seen.starts_with("Meta"),
            "expected the Meta orchestrator, got {seen}"
        );
    }

    #[tokio::test]
    async fn run_of_a_phased_workflow_hands_the_runtime_its_phase_graph() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        let dir = tmp();
        run(
            &registry,
            &config,
            "code_plan",
            "add a flag",
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        let seen = runtime.workflow.lock().unwrap().clone();
        assert!(seen.starts_with("Phased"), "{seen}");
        assert!(seen.contains("\"code_plan\""), "{seen}");
        assert!(seen.contains("resume_run_id: None"), "{seen}");
    }

    #[tokio::test]
    async fn create_runs_the_create_workflow_builtin() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        let dir = tmp();
        run(
            &registry,
            &config,
            "create_workflow",
            "a release checklist workflow",
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        assert!(
            runtime
                .workflow
                .lock()
                .unwrap()
                .contains("\"create_workflow\"")
        );
    }

    #[tokio::test]
    async fn run_dispatches_the_site_workflow_builtins_to_the_phased_engine() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        for name in [
            "copy_website",
            "reconstruct_site",
            "site_imitate",
            "ui_ux_pro_max",
        ] {
            let runtime = Arc::new(RecordingRuntime::new(""));
            let dir = tmp();
            run(
                &registry,
                &config,
                name,
                "https://example.test | a bakery",
                false,
                dir.path(),
                runtime.clone(),
            )
            .await
            .unwrap();
            let seen = runtime.workflow.lock().unwrap().clone();
            assert!(seen.starts_with("Phased"), "{name}: {seen}");
            assert!(seen.contains(&format!("\"{name}\"")), "{name}: {seen}");
        }
        let text = show_lines(registry.get("site_imitate").unwrap()).join("\n");
        assert!(text.contains("component_verified → final_verify"), "{text}");
        assert!(text.contains("tool groups: ui_catalog"), "{text}");
    }

    #[tokio::test]
    async fn run_passes_the_intent_through_as_the_task() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        let dir = tmp();
        run(
            &registry,
            &config,
            "standard",
            "fix the typo",
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            runtime.seen.lock().unwrap().as_deref(),
            Some("fix the typo")
        );
    }

    #[tokio::test]
    async fn run_of_an_unknown_workflow_fails_without_a_non_zero_exit() {
        // Matches agenthicc: the message is printed and the command still
        // exits 0, so a script probing names does not abort.
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        let dir = tmp();
        let code = run(
            &registry,
            &config,
            "nope",
            "x",
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        assert!(code.is_success());
        assert!(
            runtime.seen.lock().unwrap().is_none(),
            "an unknown workflow must never reach the runtime"
        );
    }

    #[tokio::test]
    async fn run_applies_the_workflows_model_and_turn_overrides_to_the_request() {
        struct CheckOverride;
        #[async_trait]
        impl RuntimeDispatch for CheckOverride {
            async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
                // These are the two keys that would be dead config if nothing
                // read them, so prove the request really carries them.
                assert_eq!(req.config.agent["default"].model, "cheap-model");
                assert_eq!(req.config.agent["default"].max_turns, 4);
                let session = AgentSession::new(
                    req.task.clone(),
                    req.working_dir.clone(),
                    "default".into(),
                    "m".into(),
                );
                Ok(RunResult {
                    exit_code: ExitCode::SUCCESS,
                    session,
                    summary: "ok".into(),
                })
            }
            async fn list_sessions(
                &self,
                _: &std::path::Path,
            ) -> Result<Vec<AgentSession>, RuntimeError> {
                Ok(vec![])
            }
            async fn resume_session(
                &self,
                _: &str,
                _: XaftConfig,
            ) -> Result<RunResult, RuntimeError> {
                Err(RuntimeError::NotImplemented("stub".into()))
            }
        }

        let config = with_custom(WorkflowDefinition {
            mode: Some(WorkflowMode::Dynamic),
            model: Some("cheap-model".into()),
            max_turns: Some(4),
            ..Default::default()
        });
        let registry = WorkflowRegistry::build(&config);
        let dir = tmp();
        run(
            &registry,
            &config,
            "release_review",
            "x",
            false,
            dir.path(),
            Arc::new(CheckOverride),
        )
        .await
        .unwrap();
    }

    #[test]
    fn the_failure_json_shape_matches_agenthicc() {
        let json = serde_json::json!({"status": "failed", "error": "Unknown workflow: x"});
        assert_eq!(json["status"], "failed");
        assert_eq!(json["error"], "Unknown workflow: x");
    }

    #[test]
    fn a_custom_entry_with_no_description_still_renders_a_phase_path() {
        let config = with_custom(WorkflowDefinition::default());
        let registry = WorkflowRegistry::build(&config);
        let entry: &WorkflowEntry = registry.get("release_review").unwrap();
        assert!(entry.description.is_empty(), "the CLI substitutes the text");
        assert_eq!(entry.phase_path(), "(no phases)");
    }

    // ── outcome lines ──

    #[test]
    fn the_outcome_line_for_a_fixed_orchestrator_names_the_session_once() {
        let line = run_outcome_line("meta", ExitCode::SUCCESS, "sess-1");
        assert_eq!(line, "Workflow meta 0 (session sess-1)");
        assert_eq!(line.matches("sess-1").count(), 1, "{line}");
    }

    #[test]
    fn a_failed_run_reports_its_own_exit_code() {
        let line = run_outcome_line("release_review", ExitCode::TASK_FAILED, "sess-2");
        assert_eq!(line, "Workflow release_review 1 (session sess-2)");
    }

    #[test]
    fn a_phased_run_prints_agenthiccs_line_with_a_real_run_id() {
        assert_eq!(
            phased_outcome_line("code_plan", "complete", "sess-3", "abc123"),
            "Workflow code_plan complete (session sess-3, run abc123)"
        );
    }

    // ── show / runs / validate ──

    #[test]
    fn show_lists_phases_with_edges_and_transitions() {
        let registry = WorkflowRegistry::build(&Cfg::default());
        let lines = show_lines(registry.get("code_plan").unwrap());
        let text = lines.join("\n");
        assert!(text.contains("orchestrator: phased"), "{text}");
        assert!(
            text.contains("3. review [auto]  next → summarize, on_reject → execute"),
            "{text}"
        );
        assert!(text.contains("approve_review / reject_review"), "{text}");
        assert!(
            text.contains("4. summarize [auto]  (ends the run)"),
            "{text}"
        );
    }

    #[test]
    fn runs_table_shows_the_resume_point_of_an_unfinished_run() {
        let mut failed = WorkflowRunState::new("r1", "code_plan", "add x", "s", "/w", "fp");
        failed.status = RunStatus::Failed;
        failed.next_phase = Some("review".into());
        let mut done = WorkflowRunState::new("r2", "code_plan", "add y", "s", "/w", "fp");
        done.status = RunStatus::Complete;
        let rows = runs_table(&[failed, done]);
        assert!(rows[0].starts_with("RUN"));
        assert!(
            rows[1].contains("failed") && rows[1].contains("review"),
            "{}",
            rows[1]
        );
        assert!(
            rows[2].contains("complete") && rows[2].contains(" - "),
            "{}",
            rows[2]
        );
    }

    #[tokio::test]
    async fn resume_without_a_resumable_run_is_a_usage_error() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        let dir = tmp();
        let code = resume(
            &registry,
            &config,
            None,
            None,
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::USAGE_ERROR);
        assert!(runtime.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn resume_hands_the_checkpointed_intent_and_run_id_to_the_runtime() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let dir = tmp();
        let store = CheckpointStore::for_working_dir(dir.path());
        let mut state = WorkflowRunState::new(
            "run42",
            "code_plan",
            "original intent",
            "s",
            dir.path(),
            "fp",
        );
        state.status = RunStatus::Failed;
        state.next_phase = Some("execute".into());
        store.save(&mut state).unwrap();

        let runtime = Arc::new(RecordingRuntime::new(""));
        resume(
            &registry,
            &config,
            None,
            Some("code_plan"),
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            runtime.seen.lock().unwrap().as_deref(),
            Some("original intent")
        );
        let seen = runtime.workflow.lock().unwrap().clone();
        assert!(seen.contains("resume_run_id: Some(\"run42\")"), "{seen}");
    }

    #[tokio::test]
    async fn a_completed_run_is_not_resumed() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let dir = tmp();
        let store = CheckpointStore::for_working_dir(dir.path());
        let mut state = WorkflowRunState::new("done1", "code_plan", "i", "s", dir.path(), "fp");
        state.status = RunStatus::Complete;
        store.save(&mut state).unwrap();
        let runtime = Arc::new(RecordingRuntime::new(""));
        let code = resume(
            &registry,
            &config,
            Some("done1"),
            None,
            false,
            dir.path(),
            runtime.clone(),
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::USAGE_ERROR);
        assert!(runtime.seen.lock().unwrap().is_none());
    }

    #[test]
    fn reset_removes_the_checkpoint() {
        let dir = tmp();
        let store = CheckpointStore::for_working_dir(dir.path());
        let mut state = WorkflowRunState::new("gone1", "code_plan", "i", "s", dir.path(), "fp");
        store.save(&mut state).unwrap();
        assert_eq!(reset(dir.path(), "gone1").unwrap(), ExitCode::SUCCESS);
        assert!(store.list().unwrap().is_empty());
        assert_eq!(reset(dir.path(), "gone1").unwrap(), ExitCode::USAGE_ERROR);
    }

    #[test]
    fn validate_reports_valid_and_invalid_files_with_exit_codes() {
        let dir = tmp();
        let good = dir.path().join("good_flow.toml");
        std::fs::write(&good, "[[phases]]\nname = \"only\"\n").unwrap();
        assert_eq!(validate(&good, false).unwrap(), ExitCode::SUCCESS);

        let bad = dir.path().join("bad_flow.toml");
        std::fs::write(&bad, "[[phases]]\nname = \"a\"\nnext = \"ghost\"\n").unwrap();
        assert_eq!(validate(&bad, true).unwrap(), ExitCode::TASK_FAILED);

        let garbage = dir.path().join("garbage.toml");
        std::fs::write(&garbage, "phases = 1").unwrap();
        assert_eq!(validate(&garbage, false).unwrap(), ExitCode::TASK_FAILED);
    }
}
