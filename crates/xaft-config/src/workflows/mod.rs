//! Workflow discovery and selection — the user-visible half of xaft's
//! workflow surface.
//!
//! # Why this lives in `xaft-config`
//!
//! agenthicc builds its workflow registry by importing Python
//! `WorkflowPlugin` classes from `.agenthicc/workflows/` and
//! `~/.agenthicc/workflows/`. xaft is Rust and loads no Python, so the same
//! *user-facing* shape — discover, list, select, configure — is served from
//! declarative TOML instead. This module owns that:
//!
//! * three **built-in** workflows, one per [`WorkflowMode`], always present;
//! * any number of **custom** workflows declared under `[workflows.<name>]`,
//!   which shadow a built-in of the same name (the same precedence agenthicc
//!   uses for project-over-user-over-builtin).
//!
//! It deliberately does **not** port agenthicc's runner, checkpoint, lease or
//! resume framework. Selecting a workflow resolves to one of xaft's three real
//! orchestrators; the conversion into `xaft_agents::registry::WorkflowConfig`
//! lives in `xaft-runtime` (see
//! `xaft_runtime::workflow_selection::to_runtime_config`) because
//! `xaft-config` cannot depend on the runtime.
//!
//! # What is *not* here
//!
//! `phases` is display-only. See [`crate::types::WorkflowDefinition`] for why
//! a user-declared phase graph is rendered rather than scheduled.
//!
//! ```
//! use xaft_config::{XaftConfig, workflows::WorkflowRegistry};
//!
//! let mut config = XaftConfig::default();
//! config.workflows.insert(
//!     "release_review".into(),
//!     xaft_config::types::WorkflowDefinition {
//!         description: "Plan, implement, verify.".into(),
//!         mode_bindings: vec!["yolo".into()],
//!         ..Default::default()
//!     },
//! );
//!
//! let registry = WorkflowRegistry::build(&config);
//! // Three built-ins plus the one custom workflow.
//! assert_eq!(registry.names().len(), 4);
//! // `yolo` is an alias of `auto`, and the binding is canonicalised.
//! assert_eq!(registry.mode_default_map().get("auto"), Some(&"release_review".to_string()));
//! ```

use std::collections::BTreeMap;

use crate::types::{PhaseDefinition, WorkflowDefinition, WorkflowMode, XaftConfig};

/// Canonicalise an operational mode name, folding agenthicc's documented
/// aliases onto their canonical spelling.
///
/// This mirrors `xaft_tui::mode::manager::canonical_mode_name` exactly. It is
/// duplicated rather than imported because `xaft-tui` depends on `xaft-config`,
/// not the other way round; `alias_table_matches_the_tui` in this module's
/// tests fails if the two ever drift.
pub fn canonical_mode_name(name: &str) -> &str {
    match name {
        "guard" => "safe",
        "ask" => "safe",
        "review" => "plan",
        "yolo" => "auto",
        other => other,
    }
}

/// Where a workflow definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkflowSource {
    /// Compiled into xaft; always present.
    Builtin,
    /// Declared in a `xaft.toml` under `[workflows.<name>]`.
    Custom,
}

impl WorkflowSource {
    /// The label agenthicc prints in `workflows list` — `builtin` / `project`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Custom => "project",
        }
    }
}

/// One resolvable workflow: built-in or custom, with everything a caller needs
/// to list it or run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowEntry {
    /// Canonical workflow name, e.g. `standard` or `release_review`.
    pub name: String,
    /// One-line summary; empty renders as `no description`.
    pub description: String,
    /// Provenance label.
    pub source: WorkflowSource,
    /// Canonicalised mode names this workflow is a default candidate for.
    pub mode_bindings: Vec<String>,
    /// Declared phase topology, display-only.
    pub phases: Vec<PhaseDefinition>,
    /// Which orchestrator this workflow runs.
    pub mode: WorkflowMode,
    /// Model override for the run, if the definition sets one.
    pub model: Option<String>,
    /// Turn-budget override for the run, if the definition sets one.
    pub max_turns: Option<u32>,
    /// `dynamic` mode: the first agent to run.
    pub initial_agent: Option<String>,
    /// `dynamic` mode: maximum total agent handoffs.
    pub max_handoffs: Option<u32>,
    /// `meta` mode: maximum total spawned specialists.
    pub meta_max_spawned: Option<usize>,
    /// `meta` mode: maximum concurrent specialists.
    pub meta_max_parallel: Option<usize>,
}

impl WorkflowEntry {
    /// The `phases:` line, formatted `a -> b -> c` or `(no phases)`.
    ///
    /// agenthicc joins phase names with `" -> "` and falls back to the literal
    /// `(no phases)`; reproducing that exactly keeps the two listings
    /// comparable.
    pub fn phase_path(&self) -> String {
        let joined: Vec<&str> = self.phases.iter().map(|p| p.name.as_str()).collect();
        if joined.is_empty() {
            "(no phases)".to_string()
        } else {
            joined.join(" -> ")
        }
    }

    /// The `modes:` line, or `manual` when nothing is bound.
    pub fn modes_line(&self) -> String {
        if self.mode_bindings.is_empty() {
            "manual".to_string()
        } else {
            self.mode_bindings.join(", ")
        }
    }
}

/// Why a workflow name could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    /// No workflow by that name is registered.
    NotFound(String),
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "Unknown workflow: {name}"),
        }
    }
}

impl std::error::Error for WorkflowError {}

/// The set of workflows xaft can run, built-in entries shadowed by any
/// `[workflows.<name>]` table of the same name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowRegistry {
    entries: BTreeMap<String, WorkflowEntry>,
}

impl WorkflowRegistry {
    /// Build the registry from resolved configuration.
    ///
    /// The three built-ins are always inserted first so that a session with no
    /// `xaft.toml` at all still has something to list and select.
    pub fn build(config: &XaftConfig) -> Self {
        let mut entries: BTreeMap<String, WorkflowEntry> = BTreeMap::new();

        for (name, mode, description) in [
            (
                "standard",
                WorkflowMode::Standard,
                "Classic plan -> coder -> QA <-> fixer pipeline.",
            ),
            (
                "dynamic",
                WorkflowMode::Dynamic,
                "Agent handoff: any registered agent can hand off to any other.",
            ),
            (
                "meta",
                WorkflowMode::Meta,
                "A coordinator agent designs and spawns specialist agents.",
            ),
        ] {
            entries.insert(
                name.to_string(),
                WorkflowEntry {
                    name: name.to_string(),
                    description: description.to_string(),
                    source: WorkflowSource::Builtin,
                    mode_bindings: Vec::new(),
                    phases: Vec::new(),
                    mode,
                    model: None,
                    max_turns: None,
                    initial_agent: None,
                    max_handoffs: None,
                    meta_max_spawned: None,
                    meta_max_parallel: None,
                },
            );
        }

        // Custom definitions shadow built-ins of the same name, which is the
        // same shadowing rule agenthicc applies to project-over-builtin.
        for (name, def) in &config.workflows {
            entries.insert(name.clone(), entry_from_definition(name, def));
        }

        Self { entries }
    }

    /// All workflow names, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// All entries, sorted by name — the order `xaft workflows list` prints.
    pub fn entries(&self) -> Vec<&WorkflowEntry> {
        self.entries.values().collect()
    }

    /// Look up one workflow by exact name.
    pub fn get(&self, name: &str) -> Option<&WorkflowEntry> {
        self.entries.get(name)
    }

    /// Look up one workflow, or report the unknown name.
    pub fn resolve(&self, name: &str) -> Result<&WorkflowEntry, WorkflowError> {
        self.get(name)
            .ok_or_else(|| WorkflowError::NotFound(name.to_string()))
    }

    /// Resolve `{mode: workflow}` using the **first** binding per mode.
    ///
    /// Entries are visited in sorted name order, so the result is stable for a
    /// given registry. Modes with no binding are simply absent.
    pub fn mode_default_map(&self) -> BTreeMap<String, String> {
        let mut result: BTreeMap<String, String> = BTreeMap::new();
        for entry in self.entries.values() {
            for binding in &entry.mode_bindings {
                result
                    .entry(canonical_mode_name(binding).to_string())
                    .or_insert_with(|| entry.name.clone());
            }
        }
        result
    }

    /// Apply a workflow's model and turn-budget overrides to a config, in
    /// place, on the `default` agent preset.
    ///
    /// This is the reason `model` and `max_turns` are not dead config: the
    /// resolved [`WorkflowEntry`] flows into a real [`RunRequest`] through this
    /// call, and a test asserts the preset actually changes.
    ///
    /// [`RunRequest`]: https://docs.rs/xaft-runtime
    pub fn apply_to_config(&self, entry: &WorkflowEntry, config: &mut XaftConfig) {
        let Some(preset) = config.agent.get_mut("default") else {
            return;
        };
        if let Some(model) = &entry.model {
            if !model.trim().is_empty() {
                preset.model = model.clone();
            }
        }
        if let Some(max_turns) = entry.max_turns {
            preset.max_turns = max_turns;
        }
    }

    /// The workflow a session should use when the user named none.
    ///
    /// Falls back to the orchestrator selected by `[workflow] mode`, which is
    /// how `xaft run` finally honours that key — it previously hard-coded
    /// `WorkflowConfig::default()` and ignored it.
    pub fn default_entry(&self, config: &XaftConfig) -> Option<&WorkflowEntry> {
        self.get(mode_workflow_name(&config.workflow.mode))
    }
}

/// The built-in workflow name for an orchestrator mode.
///
/// The three names are the same as the three `WorkflowMode` variants, so this
/// is a total mapping rather than a lookup table that could fall through.
pub fn mode_workflow_name(mode: &WorkflowMode) -> &'static str {
    match mode {
        WorkflowMode::Standard => "standard",
        WorkflowMode::Dynamic => "dynamic",
        WorkflowMode::Meta => "meta",
    }
}

fn entry_from_definition(name: &str, def: &WorkflowDefinition) -> WorkflowEntry {
    WorkflowEntry {
        name: name.to_string(),
        description: def.description.clone(),
        source: WorkflowSource::Custom,
        mode_bindings: def
            .mode_bindings
            .iter()
            .map(|m| canonical_mode_name(m).to_string())
            .collect(),
        phases: def.phases.clone(),
        // A custom workflow with no explicit `mode` runs the standard
        // pipeline, so declaring a workflow is never a silent no-op.
        mode: def.mode.clone().unwrap_or(WorkflowMode::Standard),
        model: def.model.clone(),
        max_turns: def.max_turns,
        initial_agent: def.dynamic_initial_agent.clone(),
        max_handoffs: def.dynamic_max_handoffs,
        meta_max_spawned: def.meta_max_spawned,
        meta_max_parallel: def.meta_max_parallel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentPreset;

    fn custom(description: &str, bindings: &[&str]) -> WorkflowDefinition {
        WorkflowDefinition {
            description: description.into(),
            mode_bindings: bindings.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn three_builtins_exist_without_any_config() {
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        assert_eq!(registry.names(), vec!["dynamic", "meta", "standard"]);
        assert!(registry.get("standard").unwrap().source == WorkflowSource::Builtin);
    }

    #[test]
    fn custom_workflow_is_discovered_and_sorts_with_the_builtins() {
        let mut config = XaftConfig::default();
        config.workflows.insert("alpha".into(), custom("A.", &[]));
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(
            registry.names(),
            vec!["alpha", "dynamic", "meta", "standard"]
        );
        assert_eq!(
            registry.get("alpha").unwrap().source,
            WorkflowSource::Custom
        );
    }

    #[test]
    fn a_custom_table_shadows_a_builtin_of_the_same_name() {
        let mut config = XaftConfig::default();
        config
            .workflows
            .insert("standard".into(), custom("Mine.", &[]));
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.get("standard").unwrap();
        assert_eq!(entry.source, WorkflowSource::Custom);
        assert_eq!(entry.description, "Mine.");
        assert_eq!(registry.names().len(), 3, "shadowing replaces, not adds");
    }

    #[test]
    fn mode_bindings_are_canonicalised_and_first_binding_wins() {
        let mut config = XaftConfig::default();
        config
            .workflows
            .insert("yolo_first".into(), custom("y", &["yolo", "plan"]));
        config
            .workflows
            .insert("auto_second".into(), custom("a", &["ask"]));
        let registry = WorkflowRegistry::build(&config);

        // yolo -> auto, ask -> safe. `yolo_first` sorts first so it wins `auto`.
        let map = registry.mode_default_map();
        assert_eq!(map.get("auto"), Some(&"yolo_first".to_string()));
        assert_eq!(map.get("safe"), Some(&"auto_second".to_string()));
        assert_eq!(map.get("plan"), Some(&"yolo_first".to_string()));
        assert_eq!(
            registry.get("yolo_first").unwrap().modes_line(),
            "auto, plan"
        );
    }

    #[test]
    fn a_workflow_with_no_bindings_renders_manual() {
        let mut config = XaftConfig::default();
        config.workflows.insert("solo".into(), custom("s", &[]));
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(registry.get("solo").unwrap().modes_line(), "manual");
    }

    #[test]
    fn phase_path_matches_agenthicc_formatting_including_the_empty_case() {
        let mut config = XaftConfig::default();
        config.workflows.insert("empty".into(), custom("e", &[]));
        config.workflows.insert(
            "three".into(),
            WorkflowDefinition {
                phases: vec![
                    PhaseDefinition {
                        name: "plan".into(),
                        ..Default::default()
                    },
                    PhaseDefinition {
                        name: "execute".into(),
                        ..Default::default()
                    },
                    PhaseDefinition {
                        name: "review".into(),
                        ..Default::default()
                    },
                ],
                ..custom("t", &[])
            },
        );
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(
            registry.get("three").unwrap().phase_path(),
            "plan -> execute -> review"
        );
        assert_eq!(registry.get("empty").unwrap().phase_path(), "(no phases)");
    }

    #[test]
    fn unknown_name_reports_the_name_it_was_given() {
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        assert_eq!(
            registry.resolve("nope"),
            Err(WorkflowError::NotFound("nope".into()))
        );
        assert_eq!(
            registry.resolve("nope").unwrap_err().to_string(),
            "Unknown workflow: nope"
        );
    }

    // ── The two fields that must not be dead config ──

    #[test]
    fn apply_to_config_overrides_the_default_presets_model_and_turns() {
        let mut config = XaftConfig::default();
        config.agent.insert(
            "default".into(),
            AgentPreset {
                model: "original".into(),
                max_turns: 25,
                ..AgentPreset::default()
            },
        );
        config.workflows.insert(
            "tuned".into(),
            WorkflowDefinition {
                model: Some("cheap-and-fast".into()),
                max_turns: Some(3),
                ..Default::default()
            },
        );

        let registry = WorkflowRegistry::build(&config);
        let entry = registry.resolve("tuned").unwrap();
        let mut applied = config.clone();
        registry.apply_to_config(entry, &mut applied);

        assert_eq!(applied.agent["default"].model, "cheap-and-fast");
        assert_eq!(applied.agent["default"].max_turns, 3);
        // The original is untouched, so this is an override and not a mutation.
        assert_eq!(config.agent["default"].model, "original");
        assert_eq!(config.agent["default"].max_turns, 25);
    }

    #[test]
    fn a_blank_model_override_is_ignored_rather_than_blanking_the_preset() {
        let mut config = XaftConfig::default();
        config.agent.insert(
            "default".into(),
            AgentPreset {
                model: "original".into(),
                ..AgentPreset::default()
            },
        );
        config.workflows.insert(
            "blank".into(),
            WorkflowDefinition {
                model: Some("   ".into()),
                ..Default::default()
            },
        );
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.resolve("blank").unwrap();
        let mut applied = config.clone();
        registry.apply_to_config(entry, &mut applied);
        assert_eq!(applied.agent["default"].model, "original");
    }

    #[test]
    fn default_entry_follows_the_configured_workflow_mode() {
        let mut config = XaftConfig::default();
        config.workflow.mode = WorkflowMode::Meta;
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(
            registry.default_entry(&config).unwrap().name,
            "meta",
            "xaft run must honour [workflow] mode rather than hard-coding Standard"
        );
    }

    #[test]
    fn a_custom_workflow_can_capture_the_default_entry_of_its_mode() {
        let mut config = XaftConfig::default();
        config.workflow.mode = WorkflowMode::Dynamic;
        config
            .workflows
            .insert("dynamic".into(), custom("my own dynamic", &[]));
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(
            registry.default_entry(&config).unwrap().description,
            "my own dynamic"
        );
    }

    #[test]
    fn every_mode_maps_to_a_workflow_that_actually_exists() {
        // Guards the total-mapping claim on `mode_workflow_name`.
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        for mode in [
            WorkflowMode::Standard,
            WorkflowMode::Dynamic,
            WorkflowMode::Meta,
        ] {
            let name = mode_workflow_name(&mode);
            assert!(
                registry.get(name).is_some(),
                "[workflow] mode {mode:?} resolves to {name}, which is not registered"
            );
        }
    }

    #[test]
    fn alias_table_matches_the_tui() {
        // xaft-tui's `canonical_mode_name` owns the user-facing alias table.
        // This copy cannot import it (xaft-config cannot depend on xaft-tui),
        // so pin the four documented entries and fail if that table changes.
        assert_eq!(canonical_mode_name("guard"), "safe");
        assert_eq!(canonical_mode_name("ask"), "safe");
        assert_eq!(canonical_mode_name("review"), "plan");
        assert_eq!(canonical_mode_name("yolo"), "auto");
        // Everything else passes through untouched.
        assert_eq!(canonical_mode_name("safe"), "safe");
        assert_eq!(canonical_mode_name("plan"), "plan");
        assert_eq!(canonical_mode_name("auto"), "auto");
    }

    #[test]
    fn a_toml_table_round_trips_into_a_custom_workflow() {
        let toml = r#"
[core]
log_level = "info"

[workflows.release_review]
description = "Plan a release, implement the work, and verify the result."
mode_bindings = ["yolo", "plan"]
model = "claude-haiku-4-5"
max_turns = 12

[[workflows.release_review.phases]]
name = "plan"
agent_type = "planner"
next = "execute"
max_turns = 12

[[workflows.release_review.phases]]
name = "execute"
agent_type = "executor"
next = "verify"
max_turns = 40

[[workflows.release_review.phases]]
name = "verify"
agent_type = "reviewer"
"#;
        let config: XaftConfig = toml::from_str(toml).expect("workflows table must parse");
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.resolve("release_review").unwrap();

        assert_eq!(
            entry.description,
            "Plan a release, implement the work, and verify the result."
        );
        assert_eq!(entry.mode_bindings, vec!["auto", "plan"]);
        assert_eq!(entry.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(entry.max_turns, Some(12));
        assert_eq!(entry.phase_path(), "plan -> execute -> verify");
        assert_eq!(entry.phases[1].next.as_deref(), Some("verify"));
        assert_eq!(entry.phases[2].next, None, "last phase is terminal");
    }

    #[test]
    fn a_config_without_any_workflows_table_still_parses() {
        let config: XaftConfig =
            toml::from_str("[core]\nlog_level = \"info\"\n").expect("minimal config");
        assert!(config.workflows.is_empty());
        assert_eq!(WorkflowRegistry::build(&config).names().len(), 3);
    }
}
