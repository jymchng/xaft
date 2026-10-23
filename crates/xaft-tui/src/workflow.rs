//! Resolve which workflow a TUI-submitted task runs under.
//!
//! The read side of `/workflow`. Three call sites must agree — the slash
//! handler, the run loop, and `/workflows` — so the mapping lives here
//! rather than being re-derived in each.

use std::path::Path;

use xaft_config::XaftConfig;
use xaft_config::workflows::{WorkflowRegistry, canonical_mode_name};
use xaft_runtime::WorkflowConfig;

/// The orchestrator for the next submitted task, from config alone.
///
/// `override_name` is the session-local `/workflow` selection, or `None`.
/// Resolution order:
///
/// 1. the named workflow, if it still resolves;
/// 2. the built-in for `[workflow] mode` (or `[workflow] default_workflow`);
/// 3. the standard pipeline — the unreachable backstop that keeps this
///    total, so a run never fails on a config problem.
pub fn resolve_for_run(override_name: Option<&str>, config: &XaftConfig) -> WorkflowConfig {
    resolve_in(
        &WorkflowRegistry::build(config),
        override_name,
        None,
        config,
    )
}

/// The orchestrator for the next TUI submit.
///
/// Like [`resolve_for_run`], but over the full registry for `working_dir`
/// (built-ins, `~/.config/xaft/workflows/`, `.xaft/workflows/`, config
/// tables) and with agenthicc's mode binding: when no `/workflow` override is
/// set, the workflow bound to the active mode (`mode_bindings`, e.g.
/// `code_plan` for `plan`) runs — agenthicc's `RuntimeMode.default_workflow`,
/// "run on user submit".
///
/// `pending_resume` is a run armed by `/workflow resume`; its workflow wins
/// over everything, and the run continues from its checkpoint.
pub fn resolve_for_submit(
    override_name: Option<&str>,
    pending_resume: Option<(String, String)>,
    config: &XaftConfig,
    working_dir: &Path,
    active_mode: Option<&str>,
) -> WorkflowConfig {
    let registry = WorkflowRegistry::discover(config, working_dir);
    if let Some((workflow, run_id)) = pending_resume {
        if let Some(entry) = registry.get(&workflow).filter(|e| e.is_phased()) {
            return WorkflowConfig::Phased {
                name: entry.name.clone(),
                definition: Box::new(entry.definition.clone()),
                resume_run_id: Some(run_id),
            };
        }
    }
    resolve_in(&registry, override_name, active_mode, config)
}

fn resolve_in(
    registry: &WorkflowRegistry,
    override_name: Option<&str>,
    active_mode: Option<&str>,
    config: &XaftConfig,
) -> WorkflowConfig {
    let mode_default = active_mode.and_then(|mode| {
        registry
            .mode_default_map()
            .get(canonical_mode_name(&mode.to_ascii_lowercase()))
            .cloned()
    });
    let entry = override_name
        .and_then(|name| registry.get(name))
        .or_else(|| mode_default.as_deref().and_then(|name| registry.get(name)))
        .or_else(|| registry.default_entry(config));
    match entry {
        Some(entry) => xaft_runtime::to_runtime_config(entry, config),
        None => WorkflowConfig::Standard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xaft_config::types::{WorkflowDefinition, WorkflowMode};

    fn label(cfg: WorkflowConfig) -> &'static str {
        match cfg {
            WorkflowConfig::Standard => "standard",
            WorkflowConfig::Dynamic { .. } => "dynamic",
            WorkflowConfig::Meta { .. } => "meta",
            WorkflowConfig::Phased { .. } => "phased",
        }
    }

    #[test]
    fn with_no_override_the_configured_mode_decides() {
        let mut config = XaftConfig::default();
        config.workflow.mode = WorkflowMode::Meta;
        assert_eq!(label(resolve_for_run(None, &config)), "meta");
    }

    #[test]
    fn the_override_beats_the_configured_mode() {
        let mut config = XaftConfig::default();
        config.workflow.mode = WorkflowMode::Meta;
        assert_eq!(
            label(resolve_for_run(Some("standard"), &config)),
            "standard"
        );
    }

    #[test]
    fn a_stale_override_falls_back_instead_of_failing_the_run() {
        // The user removed the `[workflows.gone]` table between edits; the
        // session still holds "gone". Falling back beats refusing to run.
        let config = XaftConfig::default();
        assert_eq!(label(resolve_for_run(Some("gone"), &config)), "standard");
    }

    #[test]
    fn a_custom_workflow_override_resolves_to_its_declared_mode() {
        let mut config = XaftConfig::default();
        config.workflows.insert(
            "release_review".into(),
            WorkflowDefinition {
                mode: Some(WorkflowMode::Dynamic),
                ..Default::default()
            },
        );
        assert_eq!(
            label(resolve_for_run(Some("release_review"), &config)),
            "dynamic"
        );
    }

    fn phased_name(cfg: WorkflowConfig) -> Option<(String, Option<String>)> {
        match cfg {
            WorkflowConfig::Phased {
                name,
                resume_run_id,
                ..
            } => Some((name, resume_run_id)),
            _ => None,
        }
    }

    #[test]
    fn the_plan_mode_binding_runs_code_plan_on_submit() {
        let tmp = tempfile::tempdir().unwrap();
        let config = XaftConfig::default();
        let cfg = resolve_for_submit(None, None, &config, tmp.path(), Some("plan"));
        assert_eq!(phased_name(cfg).unwrap().0, "code_plan");
        // `review` is an alias of `plan`.
        let cfg = resolve_for_submit(None, None, &config, tmp.path(), Some("review"));
        assert_eq!(phased_name(cfg).unwrap().0, "code_plan");
        // `auto` has no built-in binding, so the configured default runs.
        let cfg = resolve_for_submit(None, None, &config, tmp.path(), Some("auto"));
        assert_eq!(label(cfg), "standard");
    }

    #[test]
    fn an_explicit_override_beats_the_mode_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let config = XaftConfig::default();
        let cfg = resolve_for_submit(Some("meta"), None, &config, tmp.path(), Some("plan"));
        assert_eq!(label(cfg), "meta");
    }

    #[test]
    fn a_project_workflow_file_is_selectable_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".xaft/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("triage.toml"), "[[phases]]\nname = \"only\"\n").unwrap();
        let cfg = resolve_for_submit(
            Some("triage"),
            None,
            &XaftConfig::default(),
            tmp.path(),
            None,
        );
        assert_eq!(phased_name(cfg).unwrap().0, "triage");
    }

    #[test]
    fn an_armed_resume_wins_and_carries_its_run_id() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = resolve_for_submit(
            Some("standard"),
            Some(("code_plan".into(), "run7".into())),
            &XaftConfig::default(),
            tmp.path(),
            None,
        );
        assert_eq!(
            phased_name(cfg).unwrap(),
            ("code_plan".to_string(), Some("run7".to_string()))
        );
    }
}
