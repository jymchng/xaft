//! Resolve a [`WorkflowEntry`] into the orchestrator xaft actually runs.
//!
//! `xaft-config` owns discovery and selection but cannot name
//! [`WorkflowConfig`] — that type lives in `xaft-agents`, which depends on
//! `xaft-config`, not the other way round. This module is the single
//! translation point, so `xaft run`, `xaft workflows run` and the TUI's
//! `/workflow` all resolve a workflow identically instead of each open-coding
//! the mapping.
//!
//! ```
//! use xaft_config::{XaftConfig, workflows::WorkflowRegistry};
//! use xaft_runtime::workflow_selection::to_runtime_config;
//!
//! let registry = WorkflowRegistry::build(&XaftConfig::default());
//! // The `meta` built-in becomes a Meta orchestrator with xaft's own defaults.
//! let cfg = to_runtime_config(registry.get("meta").unwrap(), &XaftConfig::default());
//! assert!(matches!(
//!     cfg,
//!     xaft_runtime::WorkflowConfig::Meta { max_parallel_agents: 4, .. }
//! ));
//! ```

use xaft_config::types::{WorkflowMode, XaftConfig};
use xaft_config::workflows::WorkflowEntry;

use crate::agent_registry::WorkflowConfig;

/// Defaults matching `WorkflowModeConfig::default()` / `MetaWorkflowConfig`.
const DEFAULT_META_MAX_SPAWNED: usize = 8;
const DEFAULT_META_MAX_PARALLEL: usize = 4;
const DEFAULT_DYNAMIC_MAX_HANDOFFS: u32 = 12;

/// Convert a resolved entry into the runtime orchestrator.
///
/// The entry's own knobs win; anything it leaves unset falls back to the
/// matching `[workflow]` config, and finally to the compiled-in default. So a
/// custom workflow that only sets `mode = "dynamic"` still inherits the
/// project's `dynamic_max_handoffs`.
pub fn to_runtime_config(entry: &WorkflowEntry, config: &XaftConfig) -> WorkflowConfig {
    let wf = &config.workflow;
    match entry.mode {
        WorkflowMode::Standard => WorkflowConfig::Standard,
        WorkflowMode::Dynamic => {
            let initial_agent = entry
                .initial_agent
                .clone()
                .or_else(|| wf.dynamic_initial_agent.clone())
                // The dynamic registry always contains `planner`; defaulting to
                // it keeps `--workflow dynamic` runnable with no extra config.
                .unwrap_or_else(|| "planner".to_string());
            let max_handoffs = entry
                .max_handoffs
                .map(|n| n as usize)
                .or_else(|| wf.dynamic_max_handoffs)
                .unwrap_or(DEFAULT_DYNAMIC_MAX_HANDOFFS as usize);
            WorkflowConfig::Dynamic {
                initial_agent,
                max_handoffs,
                agent_subset: wf.dynamic_agent_subset.clone(),
            }
        }
        WorkflowMode::Meta => WorkflowConfig::Meta {
            meta_prompt: wf.meta_prompt.clone(),
            max_spawned_agents: entry
                .meta_max_spawned
                .or(Some(wf.meta_max_spawned))
                .unwrap_or(DEFAULT_META_MAX_SPAWNED),
            max_parallel_agents: entry
                .meta_max_parallel
                .or(Some(wf.meta_max_parallel))
                .unwrap_or(DEFAULT_META_MAX_PARALLEL),
            allow_nesting: wf.meta_allow_nesting,
            max_nesting_depth: 0,
        },
        WorkflowMode::Phased => WorkflowConfig::Phased {
            name: entry.name.clone(),
            definition: Box::new(entry.definition.clone()),
            resume_run_id: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xaft_config::types::WorkflowDefinition;
    use xaft_config::workflows::WorkflowRegistry;

    fn registry_with(def: WorkflowDefinition) -> (WorkflowRegistry, XaftConfig) {
        let mut config = XaftConfig::default();
        config.workflows.insert("custom".into(), def);
        (WorkflowRegistry::build(&config), config)
    }

    #[test]
    fn each_builtin_maps_to_its_own_orchestrator() {
        let config = XaftConfig::default();
        let registry = WorkflowRegistry::build(&config);
        assert!(matches!(
            to_runtime_config(registry.get("standard").unwrap(), &config),
            WorkflowConfig::Standard
        ));
        assert!(matches!(
            to_runtime_config(registry.get("dynamic").unwrap(), &config),
            WorkflowConfig::Dynamic { .. }
        ));
        assert!(matches!(
            to_runtime_config(registry.get("meta").unwrap(), &config),
            WorkflowConfig::Meta { .. }
        ));
    }

    #[test]
    fn a_workflow_with_no_explicit_mode_runs_the_standard_pipeline() {
        // Declaring a workflow must never be a silent no-op.
        let (registry, config) = registry_with(WorkflowDefinition {
            description: "declared but not specialised".into(),
            ..Default::default()
        });
        assert!(matches!(
            to_runtime_config(registry.get("custom").unwrap(), &config),
            WorkflowConfig::Standard
        ));
    }

    #[test]
    fn entry_knobs_win_over_the_workflow_config_section() {
        let mut config = XaftConfig::default();
        config.workflow.dynamic_max_handoffs = Some(99);
        config.workflows.insert(
            "custom".into(),
            WorkflowDefinition {
                mode: Some(WorkflowMode::Dynamic),
                dynamic_initial_agent: Some("coder".into()),
                dynamic_max_handoffs: Some(3),
                ..Default::default()
            },
        );
        let registry = WorkflowRegistry::build(&config);
        match to_runtime_config(registry.get("custom").unwrap(), &config) {
            WorkflowConfig::Dynamic {
                initial_agent,
                max_handoffs,
                ..
            } => {
                assert_eq!(initial_agent, "coder");
                assert_eq!(max_handoffs, 3);
            }
            other => panic!("expected Dynamic, got {other:?}"),
        }
    }

    #[test]
    fn unset_entry_knobs_inherit_from_the_workflow_config_section() {
        let mut config = XaftConfig::default();
        config.workflow.dynamic_initial_agent = Some("qa".into());
        config.workflow.dynamic_max_handoffs = Some(7);
        config.workflows.insert(
            "custom".into(),
            WorkflowDefinition {
                mode: Some(WorkflowMode::Dynamic),
                ..Default::default()
            },
        );
        let registry = WorkflowRegistry::build(&config);
        match to_runtime_config(registry.get("custom").unwrap(), &config) {
            WorkflowConfig::Dynamic {
                initial_agent,
                max_handoffs,
                ..
            } => {
                assert_eq!(initial_agent, "qa");
                assert_eq!(max_handoffs, 7);
            }
            other => panic!("expected Dynamic, got {other:?}"),
        }
    }

    #[test]
    fn dynamic_defaults_are_usable_with_no_configuration_at_all() {
        let config = XaftConfig::default();
        let registry = WorkflowRegistry::build(&config);
        match to_runtime_config(registry.get("dynamic").unwrap(), &config) {
            WorkflowConfig::Dynamic {
                initial_agent,
                max_handoffs,
                ..
            } => {
                assert_eq!(initial_agent, "planner");
                assert!(max_handoffs > 0, "a zero handoff budget never runs");
            }
            other => panic!("expected Dynamic, got {other:?}"),
        }
    }

    #[test]
    fn meta_limits_come_from_the_entry_when_set() {
        let (registry, config) = registry_with(WorkflowDefinition {
            mode: Some(WorkflowMode::Meta),
            meta_max_spawned: Some(2),
            meta_max_parallel: Some(1),
            ..Default::default()
        });
        match to_runtime_config(registry.get("custom").unwrap(), &config) {
            WorkflowConfig::Meta {
                max_spawned_agents,
                max_parallel_agents,
                ..
            } => {
                assert_eq!(max_spawned_agents, 2);
                assert_eq!(max_parallel_agents, 1);
            }
            other => panic!("expected Meta, got {other:?}"),
        }
    }

    #[test]
    fn meta_defaults_match_the_compiled_in_workflow_config_defaults() {
        let config = XaftConfig::default();
        assert_eq!(config.workflow.meta_max_spawned, DEFAULT_META_MAX_SPAWNED);
        assert_eq!(config.workflow.meta_max_parallel, DEFAULT_META_MAX_PARALLEL);
    }
}
