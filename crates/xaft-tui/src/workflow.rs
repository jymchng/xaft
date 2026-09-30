//! Resolve which workflow a TUI-submitted task runs under.
//!
//! The read side of `/workflow`. Three call sites must agree — the slash
//! handler, the run loop, and `/workflows` — so the mapping lives here
//! rather than being re-derived in each.

use xaft_config::XaftConfig;
use xaft_config::workflows::WorkflowRegistry;
use xaft_runtime::WorkflowConfig;

/// The orchestrator for the next submitted task.
///
/// `override_name` is the session-local `/workflow` selection, or `None`.
/// Resolution order:
///
/// 1. the named workflow, if it still resolves;
/// 2. the built-in for `[workflow] mode`;
/// 3. the standard pipeline — the unreachable backstop that keeps this
///    total, so a run never fails on a config problem.
pub fn resolve_for_run(override_name: Option<&str>, config: &XaftConfig) -> WorkflowConfig {
    let registry = WorkflowRegistry::build(config);
    let entry = override_name
        .and_then(|name| registry.get(name))
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
}
