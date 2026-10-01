//! Handler for `xaft workflows` subcommands.
//!
//! Mirrors `agenthicc.cli.commands.workflows`, which is 127 lines and does
//! exactly two things: list the registry, and run one workflow headlessly.
//!
//! # Output contracts, transcribed from agenthicc
//!
//! `workflows list` prints one three-line block per workflow, name-sorted:
//!
//! ```text
//! <name> [<source>] \u2014 <description or 'no description'>
//!   phases: a -> b -> c          (or the literal `(no phases)`)
//!   modes: safe, plan            (or the literal `manual`)
//! ```
//!
//! and `No workflows found.` when the registry is empty \u2014 unreachable here,
//! because xaft always has three built-ins, but kept so the branch is honest
//! rather than assumed away.
//!
//! `workflows run` prints
//!
//! ```text
//! Workflow <name> <status> (session <id>)
//! Summary: <summary>            (only when the run produced one)
//! ```
//!
//! Both commands support `--json`.
//!
//! # The deliberate divergences
//!
//! ## 1. `source` is `builtin` or `project`, never `user`
//!
//! agenthicc's `source` is one of `builtin` / `user` / `project`, because its
//! registry imports Python from two directories. xaft loads no Python: a
//! custom workflow is a `[workflows.<name>]` TOML table, so `source` is only
//! ever `builtin` or `project`. See
//! `xaft_config::workflows::WorkflowSource::label`.
//!
//! ## 2. There is no `run` id in the outcome line
//!
//! agenthicc prints `(session <id>, run <run-id>)`, where `run_id` is minted
//! per workflow execution (`WorkflowRunHandle.create(uuid.uuid4().hex)`) and
//! also keys its durable workflow checkpoints, so `--resume <run-id>` can
//! find it; when it has none the CLI prints the literal `unknown`.
//! xaft has no run identity to print: `RuntimeDispatch::run` returns
//! `RunResult { exit_code, session, summary }`, nothing in the runtime indexes
//! runs by id (`xaft runs` reads a `GoalRun` store that no runtime writes),
//! and even the git worktree is keyed by the session id.
//!
//! This line therefore used to print `result.session.id` twice — labelling
//! the session id `run`, which told the user nothing and implied an id they
//! could not look up anywhere. It now prints the session id once. See
//! [`run_outcome_line`] and `docs/parity-matrix.md` §1d.
//!
//! ## 3. `Summary:` replaces `Phases:` / `Error:`
//!
//! agenthicc prints `Phases: a -> b` and `Error: ...` from its own
//! `WorkflowExecutionResult`; xaft has no phase list or error field on
//! `RunResult` and substitutes the run's summary. Additive: both `Summary:`
//! lines are additional, not replacements a user can lose information on.

use std::path::PathBuf;
use std::sync::Arc;

use xaft_config::workflows::WorkflowRegistry;
use xaft_config::{CliOverrides, XaftConfig};
use xaft_runtime::ExitCode;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};

use crate::args::WorkflowsSubcommand;
use crate::error::XaftError;

/// agenthicc's `workflows list` prints this when the registry is empty.
const NO_WORKFLOWS: &str = "No workflows found.";

/// Execute `xaft workflows <subcommand>`.
pub async fn handle_workflows(
    subcommand: &WorkflowsSubcommand,
    config: &XaftConfig,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let registry = WorkflowRegistry::build(config);
    match subcommand {
        WorkflowsSubcommand::List { json } => list(&registry, *json),
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
                runtime,
            )
            .await
        }
    }
}

// ── list ────────────────────────────────────────────────────────────────────────────

fn list(registry: &WorkflowRegistry, json: bool) -> Result<ExitCode, XaftError> {
    let entries = registry.entries();

    if json {
        let payload: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "description": e.description,
                    "source": e.source.label(),
                    "mode": format!("{:?}", e.mode).to_lowercase(),
                    "mode_bindings": e.mode_bindings,
                    "model": e.model,
                    "max_turns": e.max_turns,
                    "phases": e.phases,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload).map_err(io)?);
        return Ok(ExitCode::SUCCESS);
    }

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

// ── run ─────────────────────────────────────────────────────────────────────────────

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

async fn run(
    registry: &WorkflowRegistry,
    config: &XaftConfig,
    workflow_name: &str,
    intent: &str,
    json: bool,
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

    // The workflow's own model / max_turns overrides must reach the run, which
    // is what keeps those two `[workflows.<name>]` keys from being dead config.
    let mut run_config = config.clone();
    registry.apply_to_config(entry, &mut run_config);

    let request = RunRequest {
        task: intent.to_string(),
        config: run_config,
        working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: xaft_runtime::to_runtime_config(entry, config),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    };

    let result = runtime.run(request).await?;

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
    Ok(result.exit_code)
}

/// The single line `workflows run` prints after the runtime returns.
///
/// agenthicc's shape is `Workflow <name> <status> (session <id>, run
/// <run-id>)`, where `run_id` is minted per workflow execution
/// (`WorkflowRunHandle.create(uuid.uuid4().hex)`) and also keys its durable
/// workflow checkpoint records (`agenthicc/runners/headless.py`).
///
/// xaft has no such identifier to print: `RunResult` carries `exit_code`,
/// `session` and `summary` only, nothing indexes a run by id, and even the
/// git worktree is keyed by the session id. So there is no second, distinct
/// value available here.
///
/// The alternatives were considered and rejected:
///
/// * Minting a uuid per run would print a real-looking id nothing can look
///   up — a prettier version of the same "carries no information" defect.
/// * Printing agenthicc's literal `unknown` keeps the exact string shape but
///   leaves a permanently-empty field, which reads as a bug on every run.
///
/// The session id IS the durable handle (`xaft session show <id>` and
/// `xaft sessions inspect <id>` both resolve it), so it is printed once and
/// the divergence is recorded in `docs/parity-matrix.md` §1d.
fn run_outcome_line(name: &str, exit_code: ExitCode, session_id: &str) -> String {
    format!("Workflow {name} {exit_code} (session {session_id})")
}

fn report_failure(json: bool, message: &str) {
    if json {
        let payload = serde_json::json!({"status": "failed", "error": message});
        println!("{payload}");
    } else {
        println!("Workflow failed: {message}");
    }
}

fn io(e: serde_json::Error) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use xaft_config::types::{WorkflowDefinition, WorkflowMode, XaftConfig as Cfg};
    use xaft_config::workflows::WorkflowEntry;
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

    // ── list ──

    #[test]
    fn list_always_has_the_three_builtins() {
        let registry = WorkflowRegistry::build(&Cfg::default());
        assert_eq!(registry.names(), vec!["dynamic", "meta", "standard"]);
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
        run(
            &registry,
            &config,
            "meta",
            "ship it",
            false,
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
    async fn run_passes_the_intent_through_as_the_task() {
        let config = Cfg::default();
        let registry = WorkflowRegistry::build(&config);
        let runtime = Arc::new(RecordingRuntime::new(""));
        run(
            &registry,
            &config,
            "standard",
            "fix the typo",
            false,
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
        let code = run(&registry, &config, "nope", "x", false, runtime.clone())
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
        run(
            &registry,
            &config,
            "release_review",
            "x",
            false,
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

    // ── the run id ─────────────────────────────────────────────────────
    //
    // agenthicc prints `(session <id>, run <run-id>)`. xaft used to fill
    // both fields from `result.session.id`, so the `run` value carried no
    // information and implied an id the user could not look up anywhere.
    // These pin the corrected line so it cannot drift back.

    #[test]
    fn the_outcome_line_names_the_session_and_no_pseudo_run_id() {
        let line = run_outcome_line("meta", ExitCode::SUCCESS, "sess-1");

        assert_eq!(line, "Workflow meta 0 (session sess-1)");
        assert!(
            !line.contains("run "),
            "a value labelled `run` that is not a real run id is worse than no \
             value at all: {line}"
        );
        assert_eq!(
            line.matches("sess-1").count(),
            1,
            "the session id must appear exactly once; twice means one id \
             wearing two labels: {line}"
        );
    }

    #[test]
    fn a_failed_run_reports_its_own_exit_code() {
        let line = run_outcome_line("release_review", ExitCode::TASK_FAILED, "sess-2");
        assert_eq!(line, "Workflow release_review 1 (session sess-2)");
        assert!(!line.contains("run "), "{line}");
    }

    /// The line is produced by a `println!`, which no in-process unit test can
    /// observe, so guard the format string itself. This is the exact shape
    /// that shipped; the doc comments above use `<id>` placeholders, so they
    /// cannot collide with it.
    #[test]
    fn no_line_in_this_module_prints_a_value_labelled_run() {
        // Assembled rather than written literally, so this test's own source
        // does not match the needle it is looking for.
        let needle = format!("(session {{}}, run {{}})");
        let source = include_str!("workflows.rs");
        assert!(
            !source.contains(&needle),
            "the session id must not be printed a second time under a `run` label"
        );
    }
}
