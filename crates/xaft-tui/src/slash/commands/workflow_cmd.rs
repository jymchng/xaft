//! `/workflows` and `/workflow` — discover, select and resume workflows.
//!
//! agenthicc routes `/workflow` in `TUISession.route()` *before* dispatch,
//! because the command mutates session-local workflow state. The same state
//! lives here in [`WorkflowSelection`] and is read back in `app.rs` when the
//! run request is built (`AppState::resolved_workflow`), which is what keeps
//! this from being a decorative command.
//!
//! * `/workflows` lists every workflow (built-in, user, project);
//!   `/workflows runs` lists checkpointed phased-workflow runs;
//!   `/workflows reload` re-reads the workflow directories and reports what
//!   loaded and what did not.
//! * `/workflow <name>` selects a workflow for this session; `/workflow
//!   reset` clears the selection (back to the active mode's default), and
//!   `/workflow reset <run-id>` discards that run's checkpoint.
//! * `/workflow resume [run-id]` arms a checkpointed run — the most recent
//!   resumable one by default — so the next submit continues it at the phase
//!   it had reached. Text typed with that submit (anything but a bare
//!   `continue`) reaches the resumed phase as the user's continuation, as in
//!   agenthicc's `[WORKFLOW RESUME]` prompt.

use std::sync::RwLock;

use xaft_config::XaftConfig;
use xaft_config::workflows::WorkflowRegistry;
use xaft_runtime::phased::state::{CheckpointStore, RunStatus, WorkflowRunState, snippet};

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// The session-local workflow override, shared with the run loop.
///
/// `None` means "no override" — the run follows the active mode's bound
/// workflow, then `[workflow] mode`.
#[derive(Debug, Default)]
pub struct WorkflowSelection {
    inner: RwLock<Option<String>>,
    /// `(workflow, run_id)` armed by `/workflow resume`, consumed by the next
    /// submit.
    pending_resume: RwLock<Option<(String, String)>>,
}

impl WorkflowSelection {
    /// Create a selection with no override.
    pub fn new() -> Self {
        Self::default()
    }

    /// The currently selected workflow name, if any.
    pub fn current(&self) -> Option<String> {
        self.inner.read().ok().and_then(|g| g.clone())
    }

    /// Set the override. Returns the previous value, for the confirmation text.
    pub fn set(&self, name: Option<String>) -> Option<String> {
        let mut guard = self.inner.write().ok()?;
        let previous = guard.take();
        *guard = name;
        previous
    }

    /// Arm a checkpointed run for the next submit.
    pub fn arm_resume(&self, workflow: String, run_id: String) {
        if let Ok(mut g) = self.pending_resume.write() {
            *g = Some((workflow, run_id));
        }
    }

    /// The armed run, if any, without consuming it.
    pub fn pending_resume(&self) -> Option<(String, String)> {
        self.pending_resume.read().ok().and_then(|g| g.clone())
    }

    /// Consume the armed run — called once, when a submit builds its request.
    pub fn take_pending_resume(&self) -> Option<(String, String)> {
        self.pending_resume.write().ok().and_then(|mut g| g.take())
    }
}

/// `/workflows [runs|reload]` — list workflows, runs, or reload.
pub struct WorkflowsHandler;

impl SlashHandler for WorkflowsHandler {
    fn description(&self) -> &'static str {
        "List workflows and their phases  (/workflows [runs|reload])"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[runs|reload]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        match ctx.args.trim().to_ascii_lowercase().as_str() {
            "" => list(&ctx.config, &ctx.working_dir),
            "runs" => runs(&ctx.working_dir),
            "reload" => reload(&ctx.config, &ctx.working_dir),
            other => CommandResult::Error(format!(
                "Unknown /workflows argument '{other}'. Usage: /workflows [runs|reload]"
            )),
        }
    }
}

/// `/workflow [name | reset [run-id] | resume [run-id]]`.
pub struct WorkflowHandler {
    selection: std::sync::Arc<WorkflowSelection>,
}

impl WorkflowHandler {
    pub fn new(selection: std::sync::Arc<WorkflowSelection>) -> Self {
        Self { selection }
    }
}

impl SlashHandler for WorkflowHandler {
    fn description(&self) -> &'static str {
        "Switch workflow, or resume the latest saved run  (/workflow <name> | resume [run-id] | reset [run-id])"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("<name> | resume [run-id] | reset [run-id]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let args = ctx.args.trim().to_string();
        let registry = WorkflowRegistry::discover(&ctx.config, &ctx.working_dir);
        let mut words = args.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let rest: Vec<&str> = words.collect();

        // Bare `/workflow` reports the current selection and the default.
        if args.is_empty() {
            let mut lines = vec![format!(
                "  Active workflow: {}",
                self.selection.current().unwrap_or_else(|| {
                    registry
                        .default_entry(&ctx.config)
                        .map(|e| e.name.clone())
                        .unwrap_or_else(|| "standard".into())
                })
            )];
            if let Some((wf, run)) = self.selection.pending_resume() {
                lines.push(format!(
                    "  Armed: the next submit resumes run {} of '{wf}'.",
                    short(&run)
                ));
            }
            lines.extend([
                String::new(),
                "  /workflow <name>          switch for this session".into(),
                "  /workflow resume [run]    continue a checkpointed run".into(),
                "  /workflow reset [run]     clear the selection, or discard a run".into(),
                "  /workflows [runs|reload]  list workflows or runs".into(),
            ]);
            return CommandResult::Lines(lines);
        }

        if verb.eq_ignore_ascii_case("reset") {
            if rest.len() > 1 {
                return CommandResult::Error("Usage: /workflow reset [run-id]".into());
            }
            if let Some(run_id) = rest.first() {
                return discard_run(&ctx.working_dir, run_id, &self.selection);
            }
            let previous = self.selection.set(None);
            let disarmed = self.selection.take_pending_resume().is_some();
            let mut lines = vec![match previous {
                Some(name) => format!("  Workflow override cleared (was '{name}')."),
                None => "  No workflow override was set.".to_string(),
            }];
            if disarmed {
                lines.push("  The armed resume was cancelled.".into());
            }
            return CommandResult::Lines(lines);
        }

        if verb.eq_ignore_ascii_case("resume") {
            if rest.len() > 1 {
                return CommandResult::Error("Usage: /workflow resume [run-id]".into());
            }
            return arm_resume(
                &registry,
                &ctx.working_dir,
                rest.first().copied(),
                &self.selection,
            );
        }

        match registry.resolve(&args) {
            Ok(entry) => {
                self.selection.set(Some(entry.name.clone()));
                CommandResult::Lines(vec![
                    format!("  Workflow set to '{}' for this session.", entry.name),
                    format!("    {}", entry.description),
                ])
            }
            Err(e) => CommandResult::Lines(vec![
                format!("  {e}"),
                format!("  Available: {}", registry.names().join(", ")),
            ]),
        }
    }
}

fn short(run_id: &str) -> String {
    run_id.chars().take(12).collect()
}

fn resume_point(state: &WorkflowRunState) -> String {
    state
        .next_phase
        .clone()
        .or_else(|| state.current_phase.clone())
        .unwrap_or_else(|| "-".into())
}

fn arm_resume(
    registry: &WorkflowRegistry,
    working_dir: &std::path::Path,
    run_id: Option<&str>,
    selection: &WorkflowSelection,
) -> CommandResult {
    let store = CheckpointStore::for_working_dir(working_dir);
    let state = match run_id {
        Some(id) => match store.load(id) {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("run_not_found: {e}")),
        },
        None => match store.latest_resumable(None) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return CommandResult::Lines(vec!["  No resumable workflow runs.".into()]);
            }
            Err(e) => return CommandResult::Error(e.to_string()),
        },
    };
    if !state.status.is_resumable() {
        return CommandResult::Error(format!(
            "Workflow run {} is {} and cannot be resumed.",
            short(&state.run_id),
            state.status.label()
        ));
    }
    match registry.get(&state.workflow_name) {
        Some(entry) if entry.is_phased() => {}
        _ => {
            return CommandResult::Error(format!(
                "Workflow '{}' of run {} is no longer available.",
                state.workflow_name,
                short(&state.run_id)
            ));
        }
    }
    selection.arm_resume(state.workflow_name.clone(), state.run_id.clone());
    CommandResult::Lines(vec![
        format!(
            "  ↻ Run {} of '{}' is armed: it resumes at phase '{}'.",
            short(&state.run_id),
            state.workflow_name,
            resume_point(&state)
        ),
        format!("    Intent: {}", snippet(&state.intent, 120)),
        "    Type `continue` (or guidance for the resumed phase) and press Enter.".into(),
    ])
}

fn discard_run(
    working_dir: &std::path::Path,
    run_id: &str,
    selection: &WorkflowSelection,
) -> CommandResult {
    let store = CheckpointStore::for_working_dir(working_dir);
    let state = match store.load(run_id) {
        Ok(s) => s,
        Err(e) => return CommandResult::Error(format!("run_not_found: {e}")),
    };
    if let Err(e) = store.remove(&state.run_id) {
        return CommandResult::Error(format!(
            "Cannot reset workflow run {}: {e}",
            short(&state.run_id)
        ));
    }
    if selection
        .pending_resume()
        .is_some_and(|(_, armed)| armed == state.run_id)
    {
        selection.take_pending_resume();
    }
    CommandResult::Lines(vec![format!(
        "  ↩ Workflow '{}' reset; run {} ({}) discarded.",
        state.workflow_name,
        short(&state.run_id),
        state.status.label()
    )])
}

fn runs(working_dir: &std::path::Path) -> CommandResult {
    let store = CheckpointStore::for_working_dir(working_dir);
    let list = match store.list() {
        Ok(l) => l,
        Err(e) => return CommandResult::Error(e.to_string()),
    };
    if list.is_empty() {
        return CommandResult::Lines(vec!["  No workflow runs.".into()]);
    }
    let mut lines = vec![String::new()];
    for s in list.iter().take(20) {
        let marker = if s.status.is_resumable() { "↻" } else { " " };
        let at = match s.status {
            RunStatus::Complete | RunStatus::Exited => String::new(),
            _ => format!(" at '{}'", resume_point(s)),
        };
        lines.push(format!(
            "  {marker} {}  {}  {}{at}  —  {}",
            short(&s.run_id),
            s.workflow_name,
            s.status.label(),
            snippet(&s.intent.replace('\n', " "), 60)
        ));
    }
    lines.push(String::new());
    lines.push("  /workflow resume [run-id]   continue a ↻ run".into());
    CommandResult::Lines(lines)
}

fn reload(config: &XaftConfig, working_dir: &std::path::Path) -> CommandResult {
    let registry = WorkflowRegistry::discover(config, working_dir);
    let mut lines = vec![format!(
        "  Reloaded workflows: {} available.",
        registry.names().len()
    )];
    for err in registry.load_errors() {
        lines.push(format!("  ⚠ skipped {err}"));
    }
    for bad in registry.invalid() {
        lines.push(format!("  ⚠ '{}' is invalid:", bad.name));
        for d in &bad.report.diagnostics {
            lines.push(format!("      {d}"));
        }
    }
    CommandResult::Lines(lines)
}

fn list(config: &XaftConfig, working_dir: &std::path::Path) -> CommandResult {
    let registry = WorkflowRegistry::discover(config, working_dir);
    let entries = registry.entries();
    if entries.is_empty() {
        return CommandResult::Lines(vec!["  No workflows found.".into()]);
    }
    let mut lines = vec![String::new()];
    for entry in entries {
        let description = if entry.description.is_empty() {
            "no description"
        } else {
            &entry.description
        };
        lines.push(format!(
            "  {} [{}] — {}",
            entry.name,
            entry.source.label(),
            description
        ));
        lines.push(format!("    phases: {}", entry.phase_path()));
        lines.push(format!("    modes: {}", entry.modes_line()));
    }
    for err in registry.load_errors() {
        lines.push(format!("  ⚠ skipped {err}"));
    }
    for bad in registry.invalid() {
        lines.push(format!(
            "  ⚠ '{}' is invalid (/workflows reload for details)",
            bad.name
        ));
    }
    lines.push(String::new());
    lines.push("  /workflow <name>   switch for this session".into());
    CommandResult::Lines(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::types::{PhaseDefinition, WorkflowDefinition, WorkflowMode};

    fn ctx(config: XaftConfig, command: SlashCommand, args: &str) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(config),
            session_id: None,
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
        }
    }

    fn registry_config() -> XaftConfig {
        let mut config = XaftConfig::default();
        config.workflows.insert(
            "release_review".into(),
            WorkflowDefinition {
                description: "Plan, implement, verify.".into(),
                mode_bindings: vec!["yolo".into()],
                mode: Some(WorkflowMode::Meta),
                phases: vec![
                    PhaseDefinition {
                        name: "plan".into(),
                        ..Default::default()
                    },
                    PhaseDefinition {
                        name: "verify".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        config
    }

    fn text(result: CommandResult) -> String {
        match result {
            CommandResult::Lines(lines) => lines.join("\n"),
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    fn run(args: &str, config: XaftConfig) -> (String, Arc<WorkflowSelection>) {
        let selection = Arc::new(WorkflowSelection::new());
        let command = SlashCommand::Workflow {
            name: (!args.is_empty()).then(|| args.to_string()),
        };
        let out = text(WorkflowHandler::new(selection.clone()).execute(ctx(config, command, args)));
        (out, selection)
    }

    #[test]
    fn slash_workflows_lists_the_builtins_and_a_custom_workflow() {
        let out =
            text(WorkflowsHandler.execute(ctx(registry_config(), SlashCommand::Workflows, "")));
        assert!(out.contains("standard [builtin]"), "{out}");
        assert!(
            out.contains("release_review [project] \u{2014} Plan, implement, verify."),
            "{out}"
        );
    }

    #[test]
    fn slash_workflows_renders_the_declared_phase_path() {
        let out =
            text(WorkflowsHandler.execute(ctx(registry_config(), SlashCommand::Workflows, "")));
        assert!(out.contains("phases: plan -> verify"), "{out}");
        // A built-in declares no phases and must say so rather than print nothing.
        assert!(out.contains("phases: (no phases)"), "{out}");
    }

    #[test]
    fn slash_workflows_renders_manual_for_a_workflow_with_no_mode_binding() {
        let out =
            text(WorkflowsHandler.execute(ctx(registry_config(), SlashCommand::Workflows, "")));
        assert!(out.contains("modes: manual"), "{out}");
    }

    #[test]
    fn slash_workflows_canonicalises_a_mode_binding_in_the_listing() {
        let out =
            text(WorkflowsHandler.execute(ctx(registry_config(), SlashCommand::Workflows, "")));
        // `yolo` is an alias of `auto`; the listing shows the canonical name.
        assert!(out.contains("modes: auto"), "{out}");
    }

    #[test]
    fn slash_workflow_selects_a_builtin_and_records_it() {
        let (out, selection) = run("meta", registry_config());
        assert!(out.contains("Workflow set to 'meta'"), "{out}");
        assert_eq!(selection.current().as_deref(), Some("meta"));
    }

    #[test]
    fn slash_workflow_selects_a_custom_workflow() {
        let (out, selection) = run("release_review", registry_config());
        assert!(out.contains("release_review"), "{out}");
        assert_eq!(selection.current().as_deref(), Some("release_review"));
    }

    #[test]
    fn slash_workflow_with_an_unknown_name_lists_the_options_and_changes_nothing() {
        let (out, selection) = run("nope", registry_config());
        assert!(out.contains("Unknown workflow: nope"), "{out}");
        assert!(
            out.contains("standard"),
            "the error must offer the alternatives: {out}"
        );
        assert_eq!(selection.current(), None, "a failed switch must not stick");
    }

    #[test]
    fn slash_workflow_reset_clears_the_override_and_names_what_it_cleared() {
        let (_, selection) = run("meta", registry_config());
        let out = text(WorkflowHandler::new(selection.clone()).execute(ctx(
            registry_config(),
            SlashCommand::Workflow {
                name: Some("reset".into()),
            },
            "reset",
        )));
        assert!(out.contains("override cleared (was 'meta')"), "{out}");
        assert_eq!(selection.current(), None);
    }

    #[test]
    fn slash_workflow_reset_with_no_override_is_not_an_error() {
        let selection = Arc::new(WorkflowSelection::new());
        let out = text(WorkflowHandler::new(selection.clone()).execute(ctx(
            registry_config(),
            SlashCommand::Workflow {
                name: Some("reset".into()),
            },
            "reset",
        )));
        assert!(out.contains("No workflow override was set."), "{out}");
        assert_eq!(selection.current(), None);
    }

    #[test]
    fn bare_slash_workflow_reports_the_configured_default_when_unset() {
        let mut config = registry_config();
        config.workflow.mode = WorkflowMode::Meta;
        let (out, selection) = run("", config);
        assert!(out.contains("Active workflow: meta"), "{out}");
        assert_eq!(
            selection.current(),
            None,
            "reporting the default must not itself select it"
        );
    }

    #[test]
    fn bare_slash_workflow_reports_an_existing_selection_over_the_default() {
        let mut config = registry_config();
        config.workflow.mode = WorkflowMode::Standard;
        let selection = Arc::new(WorkflowSelection::new());
        selection.set(Some("meta".into()));
        let out = text(WorkflowHandler::new(selection).execute(ctx(
            config,
            SlashCommand::Workflow { name: None },
            "",
        )));
        assert!(out.contains("Active workflow: meta"), "{out}");
    }

    #[test]
    fn switching_twice_reports_the_previous_selection_as_the_new_baseline() {
        let (first, selection) = run("standard", registry_config());
        assert!(first.contains("Workflow set to 'standard'"), "{first}");
        let out = text(WorkflowHandler::new(selection.clone()).execute(ctx(
            registry_config(),
            SlashCommand::Workflow {
                name: Some("meta".into()),
            },
            "meta",
        )));
        assert!(out.contains("Workflow set to 'meta'"), "{out}");
        assert_eq!(selection.current().as_deref(), Some("meta"));
    }

    // ── runs / resume / reset <run-id> / reload ──

    fn ctx_in(dir: &std::path::Path, command: SlashCommand, args: &str) -> CommandContext {
        let mut c = ctx(XaftConfig::default(), command, args);
        c.working_dir = dir.to_path_buf();
        c
    }

    fn workflow_cmd(args: &str) -> SlashCommand {
        SlashCommand::Workflow {
            name: Some(args.to_string()),
        }
    }

    fn save_run(dir: &std::path::Path, id: &str, status: RunStatus) {
        let mut state = WorkflowRunState::new(id, "code_plan", "add a flag", "s", dir, "fp");
        state.status = status;
        state.next_phase = Some("review".into());
        CheckpointStore::for_working_dir(dir)
            .save(&mut state)
            .unwrap();
    }

    #[test]
    fn slash_workflows_runs_lists_checkpointed_runs() {
        let tmp = tempfile::tempdir().unwrap();
        save_run(tmp.path(), "runaaa", RunStatus::Failed);
        let out =
            text(WorkflowsHandler.execute(ctx_in(tmp.path(), SlashCommand::Workflows, "runs")));
        assert!(
            out.contains("↻ runaaa  code_plan  failed at 'review'"),
            "{out}"
        );
    }

    #[test]
    fn slash_workflow_resume_arms_the_latest_resumable_run() {
        let tmp = tempfile::tempdir().unwrap();
        save_run(tmp.path(), "runbbb", RunStatus::Failed);
        let selection = Arc::new(WorkflowSelection::new());
        let out = text(WorkflowHandler::new(selection.clone()).execute(ctx_in(
            tmp.path(),
            workflow_cmd("resume"),
            "resume",
        )));
        assert!(out.contains("resumes at phase 'review'"), "{out}");
        assert_eq!(
            selection.take_pending_resume(),
            Some(("code_plan".to_string(), "runbbb".to_string()))
        );
        assert_eq!(selection.take_pending_resume(), None, "consumed once");
    }

    #[test]
    fn slash_workflow_resume_refuses_a_completed_run() {
        let tmp = tempfile::tempdir().unwrap();
        save_run(tmp.path(), "runccc", RunStatus::Complete);
        let selection = Arc::new(WorkflowSelection::new());
        let result = WorkflowHandler::new(selection.clone()).execute(ctx_in(
            tmp.path(),
            workflow_cmd("resume runccc"),
            "resume runccc",
        ));
        assert!(matches!(result, CommandResult::Error(ref e) if e.contains("cannot be resumed")));
        assert_eq!(selection.pending_resume(), None);
    }

    #[test]
    fn slash_workflow_reset_with_a_run_id_discards_the_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        save_run(tmp.path(), "runddd", RunStatus::Failed);
        let selection = Arc::new(WorkflowSelection::new());
        selection.arm_resume("code_plan".into(), "runddd".into());
        let out = text(WorkflowHandler::new(selection.clone()).execute(ctx_in(
            tmp.path(),
            workflow_cmd("reset runddd"),
            "reset runddd",
        )));
        assert!(out.contains("run runddd (failed) discarded"), "{out}");
        assert!(
            CheckpointStore::for_working_dir(tmp.path())
                .list()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            selection.pending_resume(),
            None,
            "a discarded run cannot stay armed"
        );
    }

    #[test]
    fn slash_workflows_reload_reports_broken_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".xaft/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok_flow.toml"), "[[phases]]\nname = \"a\"\n").unwrap();
        std::fs::write(
            dir.join("bad_flow.toml"),
            "[[phases]]\nname = \"a\"\nnext = \"x\"\n",
        )
        .unwrap();
        let out =
            text(WorkflowsHandler.execute(ctx_in(tmp.path(), SlashCommand::Workflows, "reload")));
        assert!(out.contains("'bad_flow' is invalid"), "{out}");
        let listing =
            text(WorkflowsHandler.execute(ctx_in(tmp.path(), SlashCommand::Workflows, "")));
        assert!(listing.contains("ok_flow [project]"), "{listing}");
    }
}
