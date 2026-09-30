//! `/workflows` and `/workflow` \u2014 discover and select the session's workflow.
//!
//! agenthicc routes `/workflow` in `TUISession.route()` *before* dispatch,
//! because the command mutates session-local workflow state. xaft has no
//! resumable phase checkpoints, so the same override reduces to: which
//! [`WorkflowConfig`] the next submitted task runs under. That is a real
//! behaviour change \u2014 it is read back in `app.rs` when the run request is
//! built, not merely printed \u2014 which is what keeps this from being a
//! decorative command.
//!
//! `/workflow reset` clears the override, matching agenthicc's
//! "clear the session-local workflow override so subsequent turns use the
//! active mode's default workflow".

use std::sync::RwLock;

use xaft_config::XaftConfig;
use xaft_config::workflows::WorkflowRegistry;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// The session-local workflow override, shared with the run loop.
///
/// `None` means "no override" \u2014 the run follows `[workflow] mode`.
#[derive(Debug, Default)]
pub struct WorkflowSelection {
    inner: RwLock<Option<String>>,
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
}

/// `/workflows` \u2014 list every workflow with its phase topology.
pub struct WorkflowsHandler;

impl SlashHandler for WorkflowsHandler {
    fn description(&self) -> &'static str {
        "List the workflows available in this session"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        list(&ctx.config)
    }
}

/// `/workflow [name|reset]` \u2014 show or switch the session's workflow.
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
        "Show or switch the active workflow  (/workflow [name|reset])"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[workflow-name|reset]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let args = ctx.args.trim().to_string();
        let registry = WorkflowRegistry::build(&ctx.config);

        // Bare `/workflow` reports the current selection and the default.
        if args.is_empty() {
            return CommandResult::Lines(vec![
                format!(
                    "  Active workflow: {}",
                    self.selection.current().unwrap_or_else(|| {
                        registry
                            .default_entry(&ctx.config)
                            .map(|e| e.name.clone())
                            .unwrap_or_else(|| "standard".into())
                    })
                ),
                String::new(),
                "  /workflow <name>   switch for this session".into(),
                "  /workflow reset   return to the configured default".into(),
                "  /workflows        list every workflow".into(),
            ]);
        }

        if args.eq_ignore_ascii_case("reset") {
            let previous = self.selection.set(None);
            return CommandResult::Lines(vec![match previous {
                Some(name) => format!("  Workflow override cleared (was '{name}')."),
                None => "  No workflow override was set.".to_string(),
            }]);
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

fn list(config: &XaftConfig) -> CommandResult {
    let registry = WorkflowRegistry::build(config);
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
}
