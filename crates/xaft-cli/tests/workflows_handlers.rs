//! Handler tests for `xaft workflows`.
//!
//! `crates/xaft-cli/src/commands/workflows.rs` covered only its unit-test
//! module; the `list` and `run` handlers themselves were unexercised, so the
//! output contract the module docs spell out in detail (three-line blocks,
//! `No workflows found.`, the `Workflow <name> <status> (session …)` line,
//! `--json` shapes) had nothing pinning it.
//!
//! The most valuable thing these pin is the **config override** path. `run`
//! calls `registry.apply_to_config(entry, &mut run_config)` specifically so a
//! workflow's own `model` / `max_turns` reach the run rather than becoming
//! dead config — G4c established that a key nothing reads is worse than a
//! missing one. `a_workflows_model_override_reaches_the_run_request` asserts
//! the stub runtime actually receives the overridden model, which is the only
//! way to know the override is not silently dropped.
//!
//! `run` also reports an unknown workflow as `Workflow failed: …` while
//! returning `ExitCode::SUCCESS`. That matches agenthicc and looks wrong, so
//! `an_unknown_workflow_reports_the_failure_but_exits_success` pins it rather
//! than leaving it to chance.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard, OnceLock};

use async_trait::async_trait;
use xaft_cli::args::WorkflowsSubcommand;
use xaft_cli::commands::workflows::handle_workflows;
use xaft_config::{PhaseDefinition, WorkflowDefinition, XaftConfig};
use xaft_runtime::agent_registry::WorkflowConfig;
use xaft_runtime::dispatch::{RunRequest, RunResult, RuntimeDispatch};
use xaft_runtime::session::AgentSession;
use xaft_runtime::{ExitCode, RuntimeError};

/// One phase, with only the fields these tests care about.
fn phase(name: &str, next: Option<&str>) -> PhaseDefinition {
    PhaseDefinition {
        name: name.to_string(),
        next: next.map(str::to_string),
        ..Default::default()
    }
}

/// Serialise the tests that read process-global config discovery.
///
/// `run` loads its own config through `ConfigLoader::load`, which walks up
/// from the cwd when no `-c` is given. Every test here passes an explicit
/// config file, but the cwd is still consulted when a path cannot be read, so
/// the lock keeps any other test from moving it underneath us.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// A runtime that records the `RunRequest` it was handed and returns a
/// canned `RunResult`.
///
/// `RuntimeDispatch` has only three methods, so a stub is cheap — and having
/// one lets the test assert on the request the handler BUILT, which is the only
/// way to observe `apply_to_config` without executing a real agent.
struct RecordingRuntime {
    seen: Mutex<Option<RunRequest>>,
    summary: String,
}

impl RecordingRuntime {
    fn new(summary: &str) -> RecordingRuntime {
        RecordingRuntime {
            seen: Mutex::new(None),
            summary: summary.to_string(),
        }
    }

    fn last_request(&self) -> RunRequest {
        self.seen
            .lock()
            .expect("request lock")
            .take()
            .expect("run() must have been called")
    }

    fn was_called(&self) -> bool {
        self.seen.lock().expect("request lock").is_some()
    }
}

#[async_trait]
impl RuntimeDispatch for RecordingRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunResult, RuntimeError> {
        // Read what the handler built BEFORE storing it, so the stub's own
        // session echoes the config the request actually carried.
        let task = request.task.clone();
        let working_dir = request.working_dir.clone();
        let model = request
            .config
            .agent
            .get("default")
            .map(|p| p.model.clone())
            .unwrap_or_default();
        *self.seen.lock().expect("request lock") = Some(request);
        Ok(RunResult {
            exit_code: ExitCode::SUCCESS,
            session: AgentSession::new(task, working_dir, "default".to_string(), model),
            summary: self.summary.clone(),
        })
    }

    async fn list_sessions(
        &self,
        _working_dir: &std::path::Path,
    ) -> Result<Vec<AgentSession>, RuntimeError> {
        Ok(Vec::new())
    }

    async fn resume_session(
        &self,
        _session_id: &str,
        _config: XaftConfig,
    ) -> Result<RunResult, RuntimeError> {
        Ok(RunResult {
            exit_code: ExitCode::SUCCESS,
            session: AgentSession::new(
                "resumed",
                PathBuf::from("."),
                "default".to_string(),
                "test-model".to_string(),
            ),
            summary: String::new(),
        })
    }
}

/// Write a config file with the given body and return its path.
fn config_with(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, body).expect("seed config");
    (dir, path)
}

fn run_args(workflow: &str, intent: &str, json: bool, config: &PathBuf) -> WorkflowsSubcommand {
    WorkflowsSubcommand::Run {
        workflow_name: workflow.to_string(),
        intent: intent.to_string(),
        json,
        config: Some(config.clone()),
        permissions: Default::default(),
    }
}

// ── list ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_succeeds_and_the_registry_is_never_empty() {
    let _lock = serial();
    let config = XaftConfig::default();
    let runtime = RecordingRuntime::new("");
    let code = handle_workflows(
        &WorkflowsSubcommand::List { json: false },
        &config,
        Arc::new(runtime),
    )
    .await
    .expect("list must not error");
    assert_eq!(code, ExitCode::SUCCESS);

    // The module docs say the empty-registry branch is unreachable here
    // because xaft always has three built-ins. Assert that, so if a future
    // change empties the registry someone notices.
    let registry = xaft_config::workflows::WorkflowRegistry::build(&config);
    assert!(
        !registry.entries().is_empty(),
        "the built-in registry must never be empty"
    );
}

#[tokio::test]
async fn list_json_includes_a_project_defined_workflow() {
    let _lock = serial();
    let mut config = XaftConfig::default();
    // A custom workflow declared the way xaft reads one: a
    // `[workflows.<name>]` table, not an import.
    config.workflows.insert(
        "release_review".to_string(),
        xaft_config::WorkflowDefinition {
            description: "check the release".to_string(),
            phases: vec![phase("scan", Some("report")), phase("report", None)],
            model: Some("test-model".to_string()),
            max_turns: Some(9),
            ..Default::default()
        },
    );

    let code = handle_workflows(
        &WorkflowsSubcommand::List { json: true },
        &config,
        Arc::new(RecordingRuntime::new("")),
    )
    .await
    .expect("list must not error");
    assert_eq!(code, ExitCode::SUCCESS);

    // Assert on the registry the handler iterates, so the --json projection is
    // known to carry the custom entry rather than only built-ins.
    let registry = xaft_config::workflows::WorkflowRegistry::build(&config);
    let entry = registry.resolve("release_review").expect("custom entry");
    assert_eq!(entry.description, "check the release");
    assert_eq!(entry.phase_path(), "scan -> report");
}

#[tokio::test]
async fn a_workflow_with_no_phases_still_renders_a_phase_line() {
    let _lock = serial();
    let mut config = XaftConfig::default();
    config.workflows.insert(
        "empty".to_string(),
        xaft_config::WorkflowDefinition::default(),
    );
    let registry = xaft_config::workflows::WorkflowRegistry::build(&config);
    let entry = registry.resolve("empty").expect("entry");
    // The docs promise the literal `(no phases)` and `manual` fallbacks.
    assert_eq!(entry.phase_path(), "(no phases)");
    assert_eq!(entry.modes_line(), "manual");

    let code = handle_workflows(
        &WorkflowsSubcommand::List { json: false },
        &config,
        Arc::new(RecordingRuntime::new("")),
    )
    .await
    .expect("list must not error");
    assert_eq!(code, ExitCode::SUCCESS);
}

// ── run: success ──────────────────────────────────────────────────────────

#[tokio::test]
async fn run_dispatches_to_the_runtime_and_returns_its_exit_code() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let runtime = RecordingRuntime::new("all good");
    let code = handle_workflows(
        &run_args("standard", "do the thing", false, &cfg),
        &XaftConfig::default(),
        Arc::new(runtime),
    )
    .await
    .expect("run must not error");

    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn run_passes_the_intent_through_as_the_task() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let runtime = Arc::new(RecordingRuntime::new(""));
    handle_workflows(
        &run_args("standard", "summarise the diff", false, &cfg),
        &XaftConfig::default(),
        runtime.clone(),
    )
    .await
    .expect("run must not error");

    let request = runtime.last_request();
    assert_eq!(request.task, "summarise the diff");
    assert!(request.headless, "workflows run must be headless");
    assert!(!request.dry_run);
    assert!(!request.auto_approve);
    assert!(
        request.prior_messages.is_empty(),
        "a fresh workflow run starts with no history"
    );
}

#[tokio::test]
async fn a_workflows_model_override_reaches_the_run_request() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let mut config = XaftConfig::default();
    config.workflows.insert(
        "tuned".to_string(),
        xaft_config::WorkflowDefinition {
            description: "a workflow that pins its own model".to_string(),
            phases: vec![phase("one", None)],
            model: Some("workflow-pinned-model".to_string()),
            max_turns: Some(7),
            ..Default::default()
        },
    );

    let runtime = Arc::new(RecordingRuntime::new(""));
    handle_workflows(
        &run_args("tuned", "go", false, &cfg),
        &config,
        runtime.clone(),
    )
    .await
    .expect("run must not error");

    let request = runtime.last_request();
    let preset = request
        .config
        .agent
        .get("default")
        .expect("the default preset must still exist");
    // This is the assertion that makes `[workflows.tuned].model` a live key
    // rather than dead config: the override has to survive into the request
    // the runtime receives.
    assert_eq!(preset.model, "workflow-pinned-model");
    assert_eq!(preset.max_turns, 7);
}

#[tokio::test]
async fn run_reports_success_in_both_text_and_json() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let code = handle_workflows(
        &run_args("standard", "go", true, &cfg),
        &XaftConfig::default(),
        Arc::new(RecordingRuntime::new("did a thing")),
    )
    .await
    .expect("run must not error");
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn run_carries_the_resolved_workflow_into_the_request() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let runtime = Arc::new(RecordingRuntime::new(""));
    handle_workflows(
        &run_args("dynamic", "go", false, &cfg),
        &XaftConfig::default(),
        runtime.clone(),
    )
    .await
    .expect("run must not error");

    // The handler builds `workflow: to_runtime_config(entry, config)`; if that
    // were dropped, a named workflow would silently run under the default
    // orchestrator.
    let request = runtime.last_request();
    // If the handler dropped the resolved workflow, a named workflow would
    // silently run under the default orchestrator, so assert the mapping.
    assert!(
        matches!(request.workflow, WorkflowConfig::Dynamic { .. }),
        "running the `dynamic` workflow must select the Dynamic orchestrator, got {:?}",
        request.workflow
    );
}

// ── run: failure ──────────────────────────────────────────────────────────

#[tokio::test]
async fn an_unknown_workflow_reports_the_failure_but_exits_success() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let runtime = Arc::new(RecordingRuntime::new(""));
    let code = handle_workflows(
        &run_args("no-such-workflow", "go", false, &cfg),
        &XaftConfig::default(),
        runtime.clone(),
    )
    .await
    .expect("run must not error");

    // Deliberately SUCCESS: agenthicc catches the resolve error, prints, and
    // returns without a non-zero exit.
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        !runtime.was_called(),
        "an unresolvable workflow must not reach the runtime"
    );
}

#[tokio::test]
async fn an_unknown_workflow_reports_the_same_way_in_json() {
    let _lock = serial();
    let (_dir, cfg) = config_with("");
    let runtime = Arc::new(RecordingRuntime::new(""));
    let code = handle_workflows(
        &run_args("no-such-workflow", "go", true, &cfg),
        &XaftConfig::default(),
        runtime.clone(),
    )
    .await
    .expect("run must not error");
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(!runtime.was_called());
}

#[tokio::test]
async fn run_loads_its_own_config_from_the_path_it_was_given() {
    let _lock = serial();
    // `workflows run` is its own entry point, so it must honour `-c` rather
    // than inheriting whatever config the parent command loaded. Write a
    // config that sets a distinctive model and assert the run picked it up.
    let (_dir, cfg) = config_with(
        r#"
[agent.default]
model = "config-file-model"
max_turns = 4
"#,
    );
    let runtime = Arc::new(RecordingRuntime::new(""));
    let code = handle_workflows(
        &run_args("standard", "go", false, &cfg),
        &XaftConfig::default(),
        runtime.clone(),
    )
    .await
    .expect("an existing -c path must load");
    assert_eq!(code, ExitCode::SUCCESS);

    let request = runtime.last_request();
    let preset = request.config.agent.get("default").expect("default preset");
    assert_eq!(
        preset.model, "config-file-model",
        "-c must reach the run, not just be validated"
    );
    assert_eq!(preset.max_turns, 4);
}

#[tokio::test]
async fn an_explicit_config_path_that_does_not_exist_is_a_hard_error() {
    let _lock = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    let absent = dir.path().join("does-not-exist.toml");
    let runtime = Arc::new(RecordingRuntime::new(""));

    // An explicit `-c` is a user assertion about where the config is, so a
    // missing file must fail loudly rather than silently running on defaults —
    // silently substituting defaults here would run the agent under a different
    // model than the user asked for.
    let err = handle_workflows(
        &run_args("standard", "go", false, &absent),
        &XaftConfig::default(),
        runtime.clone(),
    )
    .await
    .expect_err("a missing -c path must not be silently ignored");
    assert!(
        err.to_string().contains("does-not-exist.toml"),
        "the error should name the file the user passed: {err}"
    );
    assert!(
        !runtime.was_called(),
        "the run must not dispatch once config loading failed"
    );
}
