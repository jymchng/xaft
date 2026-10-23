//! Workflow discovery, validation and selection — the user-visible half of
//! xaft's workflow surface.
//!
//! # Where workflows come from
//!
//! agenthicc builds its registry from built-in `WorkflowPlugin` classes, then
//! `~/.agenthicc/workflows/`, then `.agenthicc/workflows/`. xaft does the
//! same, in the same precedence order, with declarative definitions:
//!
//! 1. **built-in** — the three fixed orchestrators (`standard`, `dynamic`,
//!    `meta`) plus the phased workflows embedded by [`builtin`]
//!    (`code_plan`, `create_workflow`, …);
//! 2. **user** — `~/.config/xaft/workflows/*.toml` and `*/workflow.toml`;
//! 3. **project** — `.xaft/workflows/*.toml` and `*/workflow.toml`;
//! 4. **config tables** — `[workflows.<name>]` in any `xaft.toml`.
//!
//! A later source shadows an earlier one of the same name. One exception
//! keeps agenthicc's `[workflows.<name>]` *parameter* tables working: a
//! config table that declares neither `phases` nor `mode` and names an
//! existing phased workflow **configures** it (models, budgets, params)
//! instead of replacing it — exactly how `[workflows.code_plan]
//! execute_model = "..."` tunes agenthicc's `CodePlanParams`.
//!
//! # What runs
//!
//! A phased definition is executed by the runtime's phased engine
//! (`xaft_runtime::phased`); the other three modes resolve to the fixed
//! orchestrators. See [`crate::types::WorkflowDefinition`] for the rules.
//! Every phased definition is checked by [`validate::validate_definition`]
//! before it is registered; one with errors is kept out of the registry and
//! reported through [`WorkflowRegistry::invalid`], so a broken file can never
//! be selected but is never silently ignored either.
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
//! assert!(registry.get("release_review").is_some());
//! assert!(registry.get("code_plan").is_some(), "built-in phased workflow");
//! // `yolo` is an alias of `auto`, and the binding is canonicalised.
//! assert_eq!(registry.mode_default_map().get("auto"), Some(&"release_review".to_string()));
//! ```

pub mod builtin;
pub mod discovery;
pub mod validate;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::types::{PhaseDefinition, WorkflowDefinition, WorkflowMode, XaftConfig};

use self::discovery::{WorkflowLoadError, load_workflow_dir};
use self::validate::{ValidationReport, validate_definition};

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
    /// A file under the user-global workflow directory.
    User,
    /// A file under the project's `.xaft/workflows/`.
    Project,
}

impl WorkflowSource {
    /// The label agenthicc prints in `workflows list` — `builtin`, `user` or
    /// `project`. A config table reads as `project`, the layer that usually
    /// declares it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Custom => "project",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// One resolvable workflow: built-in or custom, with everything a caller needs
/// to list it or run it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowEntry {
    /// Canonical workflow name, e.g. `standard` or `release_review`.
    pub name: String,
    /// One-line summary; empty renders as `no description`.
    pub description: String,
    /// Provenance label.
    pub source: WorkflowSource,
    /// The file or package directory a file-based workflow was loaded from.
    pub path: Option<PathBuf>,
    /// Canonicalised mode names this workflow is a default candidate for.
    pub mode_bindings: Vec<String>,
    /// Declared phase topology.
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
    /// The full definition — what the phased engine executes.
    pub definition: WorkflowDefinition,
    /// Validation warnings (errors keep a workflow out of the registry).
    pub warnings: Vec<String>,
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

    /// Whether this workflow runs the phased engine.
    pub fn is_phased(&self) -> bool {
        self.mode == WorkflowMode::Phased
    }
}

/// Why a workflow name could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    /// No workflow by that name is registered.
    NotFound(String),
    /// A workflow by that name exists but failed validation.
    Invalid {
        /// The workflow name.
        name: String,
        /// The rendered validation report.
        report: String,
    },
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "Unknown workflow: {name}"),
            Self::Invalid { name, report } => {
                write!(f, "Workflow '{name}' is invalid:\n{report}")
            }
        }
    }
}

impl std::error::Error for WorkflowError {}

/// A workflow that was declared but failed validation.
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidWorkflow {
    /// The declared name.
    pub name: String,
    /// Where it was declared.
    pub source: WorkflowSource,
    /// The file it came from, for file-based workflows.
    pub path: Option<PathBuf>,
    /// Every finding.
    pub report: ValidationReport,
}

/// The set of workflows xaft can run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkflowRegistry {
    entries: BTreeMap<String, WorkflowEntry>,
    load_errors: Vec<WorkflowLoadError>,
    invalid: Vec<InvalidWorkflow>,
}

impl WorkflowRegistry {
    /// Build the registry from built-ins and `[workflows.<name>]` tables only.
    ///
    /// No filesystem is read. Use [`WorkflowRegistry::discover`] to include
    /// workflow files.
    pub fn build(config: &XaftConfig) -> Self {
        Self::build_with_dirs(config, None, None)
    }

    /// Build the registry including the project's `.xaft/workflows/` under
    /// `working_dir` and the user-global workflow directory.
    pub fn discover(config: &XaftConfig, working_dir: &Path) -> Self {
        let project = discovery::project_workflow_dir(working_dir);
        let user = discovery::user_workflow_dir();
        Self::build_with_dirs(config, Some(&project), user.as_deref())
    }

    /// Build the registry from explicit workflow directories.
    ///
    /// Precedence, lowest first: built-ins, `user_dir`, `project_dir`, config
    /// tables. See the module docs for the parameter-table exception.
    pub fn build_with_dirs(
        config: &XaftConfig,
        project_dir: Option<&Path>,
        user_dir: Option<&Path>,
    ) -> Self {
        let mut registry = Self::default();

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
            let def = WorkflowDefinition {
                name: Some(name.to_string()),
                description: description.to_string(),
                mode: Some(mode),
                ..Default::default()
            };
            registry.insert(name, def, WorkflowSource::Builtin, None);
        }

        for (name, def) in builtin::builtin_phased_definitions() {
            registry.insert(&name, def, WorkflowSource::Builtin, None);
        }

        for (dir, source) in [
            (user_dir, WorkflowSource::User),
            (project_dir, WorkflowSource::Project),
        ] {
            let Some(dir) = dir else { continue };
            let (loaded, errors) = load_workflow_dir(dir);
            registry.load_errors.extend(errors);
            for wf in loaded {
                registry.insert(&wf.name, wf.definition, source, Some(wf.path));
            }
        }

        for (name, def) in &config.workflows {
            // A parameter table may address a built-in by its alias
            // (`[workflows.make_agenthicc_tool]`); it configures the
            // canonical workflow rather than declaring a new one.
            let target = match builtin::builtin_alias(name) {
                Some(canonical)
                    if !registry.entries.contains_key(name.as_str())
                        && registry.entries.contains_key(canonical) =>
                {
                    canonical
                }
                _ => name.as_str(),
            };
            let overlays_phased = def.phases.is_empty()
                && def.mode.is_none()
                && registry
                    .entries
                    .get(target)
                    .is_some_and(WorkflowEntry::is_phased);
            if overlays_phased {
                let base = registry.entries[target].clone();
                let merged = overlay_params(&base.definition, def);
                registry.insert(target, merged, base.source, base.path);
            } else {
                registry.insert(name, def.clone(), WorkflowSource::Custom, None);
            }
        }

        registry
    }

    /// Validate and register one definition, replacing any earlier entry.
    fn insert(
        &mut self,
        name: &str,
        def: WorkflowDefinition,
        source: WorkflowSource,
        path: Option<PathBuf>,
    ) {
        let report = validate_definition(name, &def);
        if !report.is_ok() {
            // A broken shadowing definition must not leave the shadowed one
            // selectable under the same name without a trace: drop both and
            // report the broken one.
            self.entries.remove(name);
            self.invalid.retain(|i| i.name != name);
            self.invalid.push(InvalidWorkflow {
                name: name.to_string(),
                source,
                path,
                report,
            });
            return;
        }
        self.invalid.retain(|i| i.name != name);
        let warnings = report.warnings().iter().map(ToString::to_string).collect();
        self.entries.insert(
            name.to_string(),
            entry_from_definition(name, &def, source, path, warnings),
        );
    }

    /// All workflow names, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// All entries, sorted by name — the order `xaft workflows list` prints.
    pub fn entries(&self) -> Vec<&WorkflowEntry> {
        self.entries.values().collect()
    }

    /// Workflow files that could not be parsed.
    pub fn load_errors(&self) -> &[WorkflowLoadError] {
        &self.load_errors
    }

    /// Declared workflows that failed validation.
    pub fn invalid(&self) -> &[InvalidWorkflow] {
        &self.invalid
    }

    /// Look up one workflow by name.
    ///
    /// A built-in alias ([`builtin::BUILTIN_ALIASES`], e.g.
    /// `make_agenthicc_tool`) resolves to its canonical entry unless a
    /// workflow by the alias's own name exists.
    pub fn get(&self, name: &str) -> Option<&WorkflowEntry> {
        self.entries.get(name).or_else(|| {
            builtin::builtin_alias(name).and_then(|canonical| self.entries.get(canonical))
        })
    }

    /// Look up one workflow, or report why it cannot be used.
    pub fn resolve(&self, name: &str) -> Result<&WorkflowEntry, WorkflowError> {
        if let Some(entry) = self.get(name) {
            return Ok(entry);
        }
        let canonical = builtin::builtin_alias(name).unwrap_or(name);
        if let Some(bad) = self
            .invalid
            .iter()
            .find(|i| i.name == name || i.name == canonical)
        {
            return Err(WorkflowError::Invalid {
                name: name.to_string(),
                report: bad.report.render(),
            });
        }
        Err(WorkflowError::NotFound(name.to_string()))
    }

    /// Entries in binding-precedence order: every user-declared workflow
    /// (config table, user or project file) before every built-in, each group
    /// sorted by name. A workflow the user bound to a mode therefore wins that
    /// mode over a built-in bound to it, such as `code_plan` for `plan`.
    fn binding_order(&self) -> impl Iterator<Item = &WorkflowEntry> {
        let custom = self
            .entries
            .values()
            .filter(|e| e.source != WorkflowSource::Builtin);
        let builtin = self
            .entries
            .values()
            .filter(|e| e.source == WorkflowSource::Builtin);
        custom.chain(builtin)
    }

    /// Resolve `{mode: workflow}` using the **first** binding per mode.
    ///
    /// Entries are visited in [`binding_order`](Self::binding_order), so the
    /// result is stable for a given registry. Modes with no binding are simply
    /// absent.
    pub fn mode_default_map(&self) -> BTreeMap<String, String> {
        let mut result: BTreeMap<String, String> = BTreeMap::new();
        for entry in self.binding_order() {
            for binding in &entry.mode_bindings {
                result
                    .entry(canonical_mode_name(binding).to_string())
                    .or_insert_with(|| entry.name.clone());
            }
        }
        result
    }

    /// Resolve `{mode: [workflow, …]}` for every binding per mode.
    pub fn mode_available_map(&self) -> BTreeMap<String, Vec<String>> {
        let mut result: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in self.binding_order() {
            for binding in &entry.mode_bindings {
                let list = result
                    .entry(canonical_mode_name(binding).to_string())
                    .or_default();
                if !list.contains(&entry.name) {
                    list.push(entry.name.clone());
                }
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
    /// `[workflow] default_workflow` wins when it names a registered workflow;
    /// otherwise the orchestrator selected by `[workflow] mode`.
    pub fn default_entry(&self, config: &XaftConfig) -> Option<&WorkflowEntry> {
        if let Some(name) = &config.workflow.default_workflow {
            if let Some(entry) = self.get(name) {
                return Some(entry);
            }
        }
        self.get(mode_workflow_name(&config.workflow.mode))
    }
}

/// The built-in workflow name for an orchestrator mode.
///
/// `phased` has no single workflow of its own, so it maps to `code_plan`, the
/// built-in phased workflow agenthicc binds to its Plan mode. Set
/// `[workflow] default_workflow` to choose another.
pub fn mode_workflow_name(mode: &WorkflowMode) -> &'static str {
    match mode {
        WorkflowMode::Standard => "standard",
        WorkflowMode::Dynamic => "dynamic",
        WorkflowMode::Meta => "meta",
        WorkflowMode::Phased => "code_plan",
    }
}

/// Merge a parameter-only config table into an existing phased definition.
fn overlay_params(base: &WorkflowDefinition, table: &WorkflowDefinition) -> WorkflowDefinition {
    let mut merged = base.clone();
    if !table.description.trim().is_empty() {
        merged.description = table.description.clone();
    }
    if !table.mode_bindings.is_empty() {
        merged.mode_bindings = table.mode_bindings.clone();
    }
    if table.model.is_some() {
        merged.model = table.model.clone();
    }
    if table.max_turns.is_some() {
        merged.max_turns = table.max_turns;
    }
    if table.max_total_phase_runs.is_some() {
        merged.max_total_phase_runs = table.max_total_phase_runs;
    }
    for (k, v) in &table.phase_models {
        merged.phase_models.insert(k.clone(), v.clone());
    }
    for (k, v) in &table.params {
        merged.params.insert(k.clone(), v.clone());
    }
    merged
}

fn entry_from_definition(
    name: &str,
    def: &WorkflowDefinition,
    source: WorkflowSource,
    path: Option<PathBuf>,
    warnings: Vec<String>,
) -> WorkflowEntry {
    WorkflowEntry {
        name: name.to_string(),
        description: def.description.clone(),
        source,
        path,
        mode_bindings: def
            .mode_bindings
            .iter()
            .map(|m| canonical_mode_name(m).to_string())
            .collect(),
        phases: def.phases.clone(),
        mode: def.resolved_mode(),
        model: def.model.clone(),
        max_turns: def.max_turns,
        initial_agent: def.dynamic_initial_agent.clone(),
        max_handoffs: def.dynamic_max_handoffs,
        meta_max_spawned: def.meta_max_spawned,
        meta_max_parallel: def.meta_max_parallel,
        definition: def.clone(),
        warnings,
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

    /// The names `WorkflowRegistry::build` registers with no config at all.
    fn builtin_names() -> Vec<&'static str> {
        let mut names = vec!["dynamic", "meta", "standard"];
        names.extend(builtin::BUILTIN_PHASED_SOURCES.iter().map(|(n, _)| *n));
        names.sort_unstable();
        names
    }

    #[test]
    fn builtins_exist_without_any_config() {
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        assert_eq!(registry.names(), builtin_names());
        assert!(registry.get("standard").unwrap().source == WorkflowSource::Builtin);
        assert!(registry.get("code_plan").unwrap().is_phased());
        assert!(registry.load_errors().is_empty());
        assert!(registry.invalid().is_empty());
    }

    #[test]
    fn custom_workflow_is_discovered_and_sorts_with_the_builtins() {
        let mut config = XaftConfig::default();
        config.workflows.insert("alpha".into(), custom("A.", &[]));
        let registry = WorkflowRegistry::build(&config);
        let mut expected = builtin_names();
        expected.push("alpha");
        expected.sort_unstable();
        assert_eq!(registry.names(), expected);
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
        assert_eq!(
            registry.names().len(),
            builtin_names().len(),
            "shadowing replaces, not adds"
        );
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
    fn with_no_user_binding_the_builtin_code_plan_is_the_plan_default() {
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        assert_eq!(
            registry.mode_default_map().get("plan"),
            Some(&"code_plan".to_string())
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
    fn a_builtin_alias_resolves_but_is_not_listed() {
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        let entry = registry.resolve("make_agenthicc_tool").unwrap();
        assert_eq!(entry.name, "make_xaft_tool");
        assert_eq!(
            registry.get("make_agenthicc_tool").unwrap().name,
            "make_xaft_tool"
        );
        assert!(!registry.names().contains(&"make_agenthicc_tool"));
    }

    #[test]
    fn a_parameter_table_under_an_alias_configures_the_canonical_builtin() {
        let mut config = XaftConfig::default();
        let mut table = WorkflowDefinition::default();
        table
            .params
            .insert("generate_model".into(), toml::Value::String("fast".into()));
        config.workflows.insert("make_agenthicc_tool".into(), table);
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.get("make_xaft_tool").unwrap();
        assert_eq!(entry.source, WorkflowSource::Builtin);
        assert_eq!(
            entry.definition.params.get("generate_model"),
            Some(&toml::Value::String("fast".into()))
        );
        assert!(!registry.names().contains(&"make_agenthicc_tool"));
    }

    #[test]
    fn a_workflow_named_like_an_alias_shadows_it() {
        let mut config = XaftConfig::default();
        config.workflows.insert(
            "make_agenthicc_tool".into(),
            WorkflowDefinition {
                mode: Some(WorkflowMode::Standard),
                ..custom("Mine.", &[])
            },
        );
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.resolve("make_agenthicc_tool").unwrap();
        assert_eq!(entry.name, "make_agenthicc_tool");
        assert_eq!(entry.description, "Mine.");
    }

    #[test]
    fn parallel_code_plan_is_bound_to_plan_but_code_plan_stays_the_default() {
        let registry = WorkflowRegistry::build(&XaftConfig::default());
        assert_eq!(
            registry.mode_default_map().get("plan"),
            Some(&"code_plan".to_string())
        );
        assert_eq!(
            registry.mode_available_map()["plan"],
            ["code_plan", "parallel_code_plan"]
        );
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
            WorkflowMode::Phased,
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
        assert_eq!(
            WorkflowRegistry::build(&config).names().len(),
            builtin_names().len()
        );
    }

    // ── Files, overlays and validation ──

    const FILE_FLOW: &str = r#"
description = "From a file."
[[phases]]
name = "only"
"#;

    #[test]
    fn project_files_shadow_user_files_and_builtins() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(user.join("shared.toml"), FILE_FLOW).unwrap();
        std::fs::write(
            project.join("shared.toml"),
            FILE_FLOW.replace("From a file.", "Project wins."),
        )
        .unwrap();
        std::fs::write(user.join("mine.toml"), FILE_FLOW).unwrap();

        let registry =
            WorkflowRegistry::build_with_dirs(&XaftConfig::default(), Some(&project), Some(&user));
        let shared = registry.get("shared").unwrap();
        assert_eq!(shared.description, "Project wins.");
        assert_eq!(shared.source, WorkflowSource::Project);
        assert_eq!(shared.source.label(), "project");
        assert!(shared.path.as_ref().unwrap().ends_with("shared.toml"));
        let mine = registry.get("mine").unwrap();
        assert_eq!(mine.source.label(), "user");
        assert!(mine.is_phased());
    }

    #[test]
    fn discover_reads_the_project_workflow_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = discovery::project_workflow_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("found_me.toml"), FILE_FLOW).unwrap();
        let registry = WorkflowRegistry::discover(&XaftConfig::default(), tmp.path());
        assert!(registry.get("found_me").is_some());
    }

    #[test]
    fn a_parameter_table_configures_a_phased_builtin_instead_of_replacing_it() {
        let config: XaftConfig = toml::from_str(
            "[workflows.code_plan]\nexecute_model = \"cheap\"\nmax_total_phase_runs = 9\n",
        )
        .unwrap();
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.get("code_plan").unwrap();
        assert!(entry.is_phased(), "still the built-in phase graph");
        assert_eq!(entry.phases.len(), 4);
        assert_eq!(entry.source, WorkflowSource::Builtin);
        let execute = entry.phases.iter().find(|p| p.name == "execute").unwrap();
        assert_eq!(
            entry.definition.model_for_phase(execute).as_deref(),
            Some("cheap")
        );
        assert_eq!(entry.definition.max_total_phase_runs, Some(9));
    }

    #[test]
    fn agenthicc_parameter_tables_configure_the_site_workflows() {
        let config: XaftConfig = toml::from_str(
            "[workflows.copy_website]\nstudy_model = \"cheap\"\n\
             [workflows.ui_ux_pro_max]\nmax_phase_turns = 9\nmax_remediation_cycles = 1\n\
             [workflows.reconstruct_site]\nprofile = \"production\"\ndesign_model = \"x\"\n\
             custom_phases = [\"init\", \"research_gate\", \"final_validation\"]\n",
        )
        .unwrap();
        let registry = WorkflowRegistry::build(&config);
        for name in [
            "copy_website",
            "ui_ux_pro_max",
            "reconstruct_site",
            "site_imitate",
        ] {
            let entry = registry.get(name).unwrap();
            assert!(entry.is_phased(), "{name} keeps its phase graph");
            assert!(entry.warnings.is_empty(), "{name}: {:?}", entry.warnings);
        }
        let copy = registry.get("copy_website").unwrap();
        let study = copy.phases.iter().find(|p| p.name == "site_study").unwrap();
        assert_eq!(
            copy.definition.model_for_phase(study).as_deref(),
            Some("cheap")
        );
        let ux = registry.get("ui_ux_pro_max").unwrap();
        assert_eq!(ux.definition.max_turns_for_phase(&ux.phases[3]), 9);
        let rebuild = registry.get("reconstruct_site").unwrap();
        let design = rebuild
            .phases
            .iter()
            .find(|p| p.name == "design_system")
            .unwrap();
        assert_eq!(
            rebuild.definition.model_for_phase(design).as_deref(),
            Some("x")
        );
    }

    #[test]
    fn a_table_with_phases_replaces_the_builtin_of_the_same_name() {
        let config: XaftConfig = toml::from_str(
            "[workflows.code_plan]\n[[workflows.code_plan.phases]]\nname = \"solo\"\n",
        )
        .unwrap();
        let registry = WorkflowRegistry::build(&config);
        let entry = registry.get("code_plan").unwrap();
        assert_eq!(entry.phase_path(), "solo");
        assert_eq!(entry.source, WorkflowSource::Custom);
    }

    #[test]
    fn an_invalid_workflow_is_reported_and_unselectable() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("broken.toml"),
            "[[phases]]\nname = \"a\"\nnext = \"ghost\"\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("garbage.toml"), "phases = 1").unwrap();
        let registry =
            WorkflowRegistry::build_with_dirs(&XaftConfig::default(), Some(tmp.path()), None);
        assert!(registry.get("broken").is_none());
        assert_eq!(registry.invalid().len(), 1);
        assert_eq!(registry.load_errors().len(), 1);
        let err = registry.resolve("broken").unwrap_err();
        assert!(matches!(err, WorkflowError::Invalid { .. }));
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[test]
    fn default_workflow_key_wins_over_mode() {
        let mut config = XaftConfig::default();
        config.workflow.default_workflow = Some("create_workflow".into());
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(
            registry.default_entry(&config).unwrap().name,
            "create_workflow"
        );
        config.workflow.default_workflow = Some("not_registered".into());
        assert_eq!(registry.default_entry(&config).unwrap().name, "standard");
    }

    #[test]
    fn phases_without_a_mode_run_the_phased_engine() {
        let mut config = XaftConfig::default();
        config.workflows.insert(
            "graph".into(),
            WorkflowDefinition {
                phases: vec![PhaseDefinition {
                    name: "only".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        config.workflows.insert(
            "described".into(),
            WorkflowDefinition {
                mode: Some(WorkflowMode::Standard),
                phases: vec![PhaseDefinition {
                    name: "only".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let registry = WorkflowRegistry::build(&config);
        assert_eq!(registry.get("graph").unwrap().mode, WorkflowMode::Phased);
        assert_eq!(
            registry.get("described").unwrap().mode,
            WorkflowMode::Standard,
            "an explicit mode keeps phases descriptive"
        );
    }

    #[test]
    fn mode_available_map_lists_every_binding() {
        let mut config = XaftConfig::default();
        config
            .workflows
            .insert("one".into(), custom("1", &["plan"]));
        let registry = WorkflowRegistry::build(&config);
        let map = registry.mode_available_map();
        assert_eq!(
            map.get("plan").unwrap(),
            &vec![
                "one".to_string(),
                "code_plan".to_string(),
                "parallel_code_plan".to_string()
            ],
            "user bindings are listed before built-in ones"
        );
    }
}
