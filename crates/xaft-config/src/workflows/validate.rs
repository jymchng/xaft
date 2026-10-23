//! Deterministic validation of a phased workflow definition.
//!
//! agenthicc validates a generated workflow by importing it and inspecting the
//! plugin class (`create_workflow/validation.py`). xaft workflows are data, so
//! the same guarantees are checked structurally instead: every edge names a
//! real phase, every enum-like field holds a known value, the graph terminates,
//! and every declared transition tool is well formed.
//!
//! The validator never touches the filesystem and never runs a model, so the
//! registry, `xaft workflows validate` and the `create_workflow` authoring
//! tools all report exactly the same diagnostics for the same definition.
//!
//! ```
//! use xaft_config::types::{PhaseDefinition, WorkflowDefinition, WorkflowMode};
//! use xaft_config::workflows::validate::validate_definition;
//!
//! let def = WorkflowDefinition {
//!     mode: Some(WorkflowMode::Phased),
//!     phases: vec![PhaseDefinition {
//!         name: "plan".into(),
//!         next: Some("missing".into()),
//!         ..Default::default()
//!     }],
//!     ..Default::default()
//! };
//! let report = validate_definition("demo", &def);
//! assert!(!report.is_ok());
//! assert!(report.errors()[0].message.contains("missing"));
//! ```

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{PhaseDefinition, WorkflowDefinition};

/// Agent roles a phase may name, mirroring agenthicc's `PhaseRole` plus the
/// names of xaft's own built-in agents.
pub const KNOWN_AGENT_TYPES: &[&str] = &[
    "auto", "planner", "executor", "reviewer", "explorer", "verifier", "human", "script", "custom",
    "coder", "qa", "fixer",
];

/// Tool capabilities a phase may allow.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "read",
    "search",
    "write",
    "execute",
    "git_read",
    "git_write",
    "network",
    "memory",
    "control",
];

/// Output schemas understood by the engine.
pub const KNOWN_OUTPUT_SCHEMAS: &[&str] = &["plan", "review_result", "free_text", "json"];

/// Operational modes a phase may switch to (canonical names and aliases).
pub const KNOWN_MODES: &[&str] = &[
    "auto", "yolo", "plan", "review", "safe", "guard", "ask", "debug",
];

/// Named tool groups a phase may add through `extra_tools`.
///
/// The runtime owns the implementations; the names live here so a definition
/// can be validated without depending on the runtime crate.
pub const KNOWN_TOOL_GROUPS: &[&str] = &[
    "workflow_authoring",
    "goal_list",
    "workflow_state",
    "artifacts",
    "ask_user",
    "parallel_workers",
    "custom_tool_authoring",
];

/// Guards a transition tool may declare.
pub const KNOWN_GUARDS: &[&str] = &[
    "goals_complete",
    "goals_pending",
    "has_artifacts",
    "plan_approved",
    "draft_valid",
    "tool_draft_valid",
];

/// Deterministic effects a transition tool may declare.
pub const KNOWN_EFFECTS: &[&str] = &[
    "publish_workflow_draft",
    // goal_flow: the goal list and its cursor.
    "finalize_goals",
    "goal_implemented",
    "goal_verdict",
    // make_xaft_tool: plan, draft and publication of a persistent custom tool.
    "record_tool_plan",
    "record_tool_draft",
    "publish_custom_tool",
];

/// JSON types a transition parameter may declare (`type = "..."`).
pub const KNOWN_PARAM_TYPES: &[&str] = &["string", "boolean", "integer", "array", "object"];

/// Item types an `array` transition parameter may declare (`items = "..."`).
pub const KNOWN_PARAM_ITEMS: &[&str] = &["string", "object", "any"];

/// Transition outcomes.
pub const KNOWN_OUTCOMES: &[&str] = &["approved", "rejected", "exit"];

/// Tool names reserved by the engine's built-in transition tools.
pub const RESERVED_TOOL_NAMES: &[&str] = &[
    "request_plan_approval",
    "finalize_plan",
    "exit_workflow",
    "mark_execute_complete",
    "approve_review",
    "reject_review",
];

/// How bad a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The workflow cannot run.
    Error,
    /// The workflow runs, but probably not as intended.
    Warning,
}

impl Severity {
    /// `error` / `warning`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// The phase the finding is about, if any.
    pub phase: Option<String>,
    /// Human-readable explanation, including what to change.
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.phase {
            Some(phase) => write!(
                f,
                "{}: phase '{}': {}",
                self.severity.label(),
                phase,
                self.message
            ),
            None => write!(f, "{}: {}", self.severity.label(), self.message),
        }
    }
}

/// Every finding for one definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// All findings, errors first.
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    /// `true` when there are no errors (warnings are allowed).
    pub fn is_ok(&self) -> bool {
        self.errors().is_empty()
    }

    /// The error findings.
    pub fn errors(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect()
    }

    /// The warning findings.
    pub fn warnings(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect()
    }

    /// One line per finding, or `ok` when there are none.
    pub fn render(&self) -> String {
        if self.diagnostics.is_empty() {
            return "ok".to_string();
        }
        self.diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn error(&mut self, phase: Option<&str>, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            phase: phase.map(str::to_string),
            message: message.into(),
        });
    }

    fn warn(&mut self, phase: Option<&str>, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            phase: phase.map(str::to_string),
            message: message.into(),
        });
    }
}

/// Whether `s` is a `lower_snake_case` identifier.
pub fn is_snake_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Validate a workflow definition.
///
/// Non-phased definitions (`standard` / `dynamic` / `meta`) only have their
/// name checked: their phases are descriptive.
pub fn validate_definition(name: &str, def: &WorkflowDefinition) -> ValidationReport {
    let mut report = ValidationReport::default();

    if !is_snake_identifier(name) {
        report.error(
            None,
            format!("workflow name '{name}' must be lower_snake_case (e.g. 'release_review')"),
        );
    }

    if !def.is_phased() {
        return report;
    }

    if def.phases.is_empty() {
        report.error(
            None,
            "a phased workflow needs at least one [[phases]] entry",
        );
        return report;
    }

    // ── Names ────────────────────────────────────────────────────────────────
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for phase in &def.phases {
        let pname = phase.name.as_str();
        if pname.trim().is_empty() {
            report.error(None, "every phase needs a non-empty name");
            continue;
        }
        if !is_snake_identifier(pname) {
            report.error(
                Some(pname),
                "phase names must be lower_snake_case identifiers",
            );
        }
        if !seen.insert(pname) {
            report.error(Some(pname), "phase name is declared more than once");
        }
    }
    let names: BTreeSet<&str> = def.phases.iter().map(|p| p.name.as_str()).collect();

    for key in def.phase_models.keys() {
        if !names.contains(key.as_str()) {
            report.warn(
                None,
                format!("phase_models names '{key}', which is not a phase of this workflow"),
            );
        }
    }
    // A phase may name the param that sets its model (`model_key`), as
    // agenthicc's goal_flow reads `implement_model` for `implement_goal`.
    let model_keys: BTreeSet<&str> = def
        .phases
        .iter()
        .filter_map(|p| p.model_key.as_deref())
        .collect();
    for (key, value) in &def.params {
        if let Some(phase) = key.strip_suffix("_model") {
            if !names.contains(phase) && !model_keys.contains(key.as_str()) {
                report.warn(
                    None,
                    format!("param '{key}' looks like a phase model but '{phase}' is not a phase"),
                );
            } else if !value.is_str() {
                report.error(None, format!("param '{key}' must be a string model id"));
            }
        } else if model_keys.contains(key.as_str()) && !value.is_str() {
            report.error(None, format!("param '{key}' must be a string model id"));
        }
    }

    // ── Per-phase checks ─────────────────────────────────────────────────────
    let facts = WorkflowFacts::of(def);
    for phase in &def.phases {
        check_phase(phase, &names, &facts, &mut report);
    }

    // ── Graph checks ─────────────────────────────────────────────────────────
    check_reachability(def, &mut report);
    check_unbounded_cycles(def, &mut report);
    check_skip_cycles(def, &mut report);

    report.diagnostics.sort_by_key(|d| d.severity);
    report
}

/// What the whole workflow provides, for checks that one phase cannot answer
/// alone (a goal list recorded in one phase is guarded on in another).
struct WorkflowFacts {
    /// Some phase adds `goal_list`, or some transition records goals.
    records_goals: bool,
}

impl WorkflowFacts {
    fn of(def: &WorkflowDefinition) -> Self {
        Self {
            records_goals: def.phases.iter().any(|p| {
                p.extra_tools.iter().any(|g| g == "goal_list")
                    || p.transitions
                        .iter()
                        .any(|t| t.effect.as_deref() == Some("finalize_goals"))
            }),
        }
    }
}

fn check_phase(
    phase: &PhaseDefinition,
    names: &BTreeSet<&str>,
    facts: &WorkflowFacts,
    report: &mut ValidationReport,
) {
    let p = Some(phase.name.as_str());

    let role = phase.role();
    if !KNOWN_AGENT_TYPES.contains(&role) {
        report.warn(
            p,
            format!(
                "agent_type '{role}' is not a built-in role ({}); it runs with the 'auto' role prompt",
                KNOWN_AGENT_TYPES.join(", ")
            ),
        );
    }

    for (label, target) in [
        ("next", &phase.next),
        ("on_reject", &phase.on_reject),
        ("on_error", &phase.on_error),
    ] {
        if let Some(t) = target {
            if !names.contains(t.as_str()) {
                report.error(p, format!("{label} = '{t}' does not name a phase"));
            }
        }
    }

    for peer in &phase.parallel_with {
        if peer == &phase.name {
            report.error(p, "parallel_with must not name the phase itself");
        } else if !names.contains(peer.as_str()) {
            report.error(
                p,
                format!("parallel_with names '{peer}', which is not a phase"),
            );
        }
    }

    if let Some(max_turns) = phase.max_turns {
        if max_turns == 0 {
            report.error(p, "max_turns must be at least 1");
        }
    }
    if let Some(n) = phase.max_iterations {
        if n == 0 || n < -1 {
            report.error(
                p,
                "max_iterations must be -1 (unlimited) or a positive integer",
            );
        }
    }
    if phase.max_attempts == Some(0) {
        report.error(p, "max_attempts must be at least 1");
    }

    match (&phase.skip_when, &phase.skip_to) {
        (Some(guard), skip_to) => {
            if !KNOWN_GUARDS.contains(&guard.as_str()) {
                report.error(
                    p,
                    format!(
                        "skip_when guard '{guard}' is unknown; use {}",
                        KNOWN_GUARDS.join(", ")
                    ),
                );
            }
            if guard.starts_with("goals_") && !facts.records_goals {
                report.warn(
                    p,
                    format!(
                        "skip_when = '{guard}' but no phase adds the goal_list tool group, \
                         so no goal is ever recorded"
                    ),
                );
            }
            match skip_to {
                None => report.error(p, "skip_when needs skip_to: the phase to continue at"),
                Some(target) if target == &phase.name => {
                    report.error(p, "skip_to must not name the phase itself");
                }
                Some(target) if !names.contains(target.as_str()) => {
                    report.error(p, format!("skip_to = '{target}' does not name a phase"));
                }
                Some(_) => {}
            }
        }
        (None, Some(_)) => report.error(p, "skip_to is only valid together with skip_when"),
        (None, None) => {}
    }

    if let Some(key) = &phase.model_key {
        if !is_snake_identifier(key) {
            report.error(
                p,
                format!("model_key '{key}' must be a lower_snake_case param name"),
            );
        }
    }

    if let Some(schema) = &phase.output_schema {
        if !KNOWN_OUTPUT_SCHEMAS.contains(&schema.as_str()) {
            report.error(
                p,
                format!(
                    "output_schema '{schema}' is unknown; use one of {}",
                    KNOWN_OUTPUT_SCHEMAS.join(", ")
                ),
            );
        }
    }

    if let Some(mode) = &phase.mode_override {
        if !KNOWN_MODES.contains(&mode.to_ascii_lowercase().as_str()) {
            report.error(
                p,
                format!(
                    "mode_override '{mode}' is not a known mode ({})",
                    KNOWN_MODES.join(", ")
                ),
            );
        }
    }

    if let Some(caps) = &phase.allowed_capabilities {
        for cap in caps {
            if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
                report.error(
                    p,
                    format!(
                        "allowed_capabilities entry '{cap}' is unknown; use {}",
                        KNOWN_CAPABILITIES.join(", ")
                    ),
                );
            }
        }
    }

    for group in &phase.extra_tools {
        if !KNOWN_TOOL_GROUPS.contains(&group.as_str()) {
            report.error(
                p,
                format!(
                    "extra_tools group '{group}' is unknown; available groups: {}",
                    KNOWN_TOOL_GROUPS.join(", ")
                ),
            );
        }
    }

    // agenthicc's PhaseSpec.__post_init__ invariants.
    let wait = phase
        .terminal_wait_policy
        .as_deref()
        .unwrap_or("foreground");
    let lifecycle = phase.command_lifecycle.as_deref().unwrap_or("oneshot");
    if !matches!(wait, "foreground" | "background") {
        report.error(
            p,
            "terminal_wait_policy must be 'foreground' or 'background'",
        );
    }
    if !matches!(lifecycle, "oneshot" | "service") {
        report.error(p, "command_lifecycle must be 'oneshot' or 'service'");
    }
    if lifecycle == "service" && wait != "background" {
        report.error(
            p,
            "command_lifecycle = 'service' requires terminal_wait_policy = 'background'",
        );
    }
    if phase.require_readiness && lifecycle != "service" {
        report.error(
            p,
            "require_readiness requires command_lifecycle = 'service'",
        );
    }

    let flags = [
        phase.require_plan_finalization,
        phase.require_explicit_completion,
        phase.require_explicit_review,
    ]
    .iter()
    .filter(|f| **f)
    .count();
    if flags > 1 {
        report.warn(
            p,
            "more than one require_* flag is set; precedence is plan_finalization, then \
             explicit_completion, then explicit_review",
        );
    }
    if flags > 0 && !phase.transitions.is_empty() {
        report.warn(
            p,
            "custom transitions are offered alongside the require_* tools; either call ends the phase",
        );
    }

    if phase.is_script() {
        match phase.command.as_deref().map(str::trim) {
            None | Some("") => {
                report.error(p, "a script phase needs a non-empty `command`");
            }
            Some(_) => {}
        }
        if phase.timeout_s == Some(0) {
            report.error(p, "timeout_s must be at least 1");
        }
        if flags > 0 || !phase.transitions.is_empty() || !phase.extra_tools.is_empty() {
            report.warn(
                p,
                "script phases run a command, not an agent: require_* flags, transitions and \
                 extra_tools are ignored (the command chooses its edge with its JSON output)",
            );
        }
    } else if phase.command.is_some() || phase.timeout_s.is_some() {
        report.error(
            p,
            "`command` / `timeout_s` are only valid with agent_type = \"script\"",
        );
    }

    if phase.is_human() {
        if phase.prompt.trim().is_empty() && phase.prompt_file.is_none() {
            report.warn(
                p,
                "a human phase has no prompt; the user will be asked to approve the work so far",
            );
        }
        if flags > 0 || !phase.transitions.is_empty() {
            report.warn(
                p,
                "human phases do not run an agent, so require_* flags and transitions are ignored",
            );
        }
    }

    let can_reject = phase.require_explicit_review
        || phase.require_plan_finalization
        || phase.is_human()
        || phase.is_script()
        || phase.output_schema.as_deref() == Some("review_result")
        // A goal_list phase control of the active goal (postpone/skip/delete)
        // rejects the phase's work on that goal.
        || phase.extra_tools.iter().any(|g| g == "goal_list")
        || phase
            .transitions
            .iter()
            .any(|t| t.outcome_or_default() == "rejected");
    if phase.on_reject.is_some() && !can_reject {
        report.warn(
            p,
            "on_reject is set but nothing in this phase can reject (add require_explicit_review, \
             a review_result output_schema, or a transition with outcome = 'rejected')",
        );
    }

    // Transitions.
    let mut tool_names: BTreeSet<&str> = BTreeSet::new();
    for t in &phase.transitions {
        let tname = t.tool.as_str();
        if !is_snake_identifier(tname) {
            report.error(
                p,
                format!("transition tool '{tname}' must be a lower_snake_case identifier"),
            );
        }
        if RESERVED_TOOL_NAMES.contains(&tname) {
            report.error(
                p,
                format!("transition tool '{tname}' collides with a built-in transition tool"),
            );
        }
        if !tool_names.insert(tname) {
            report.error(p, format!("transition tool '{tname}' is declared twice"));
        }
        if t.description.trim().is_empty() {
            report.warn(
                p,
                format!("transition tool '{tname}' has no description; the model will not know when to call it"),
            );
        }
        if let Some(target) = &t.next {
            if !names.contains(target.as_str()) {
                report.error(
                    p,
                    format!("transition '{tname}' targets '{target}', which is not a phase"),
                );
            }
            if t.end {
                report.error(
                    p,
                    format!("transition '{tname}' sets both next and end = true"),
                );
            }
        }
        let outcome = t.outcome_or_default();
        if !KNOWN_OUTCOMES.contains(&outcome) {
            report.error(
                p,
                format!(
                    "transition '{tname}' has outcome '{outcome}'; use {}",
                    KNOWN_OUTCOMES.join(", ")
                ),
            );
        }
        if let Some(guard) = &t.guard {
            if !KNOWN_GUARDS.contains(&guard.as_str()) {
                report.error(
                    p,
                    format!(
                        "transition '{tname}' has unknown guard '{guard}'; use {}",
                        KNOWN_GUARDS.join(", ")
                    ),
                );
            }
            if guard.starts_with("goals_") && !facts.records_goals {
                report.warn(
                    p,
                    format!(
                        "transition '{tname}' guards on {guard} but no phase adds the \
                         goal_list tool group, so no goal is ever recorded"
                    ),
                );
            }
        }
        if let Some(effect) = &t.effect {
            if !KNOWN_EFFECTS.contains(&effect.as_str()) {
                report.error(
                    p,
                    format!(
                        "transition '{tname}' has unknown effect '{effect}'; use {}",
                        KNOWN_EFFECTS.join(", ")
                    ),
                );
            }
        }
        let mut params: BTreeSet<&str> = BTreeSet::new();
        for param in &t.params {
            if !is_snake_identifier(&param.name) {
                report.error(
                    p,
                    format!(
                        "transition '{tname}' parameter '{}' must be lower_snake_case",
                        param.name
                    ),
                );
            }
            if !params.insert(param.name.as_str()) {
                report.error(
                    p,
                    format!(
                        "transition '{tname}' declares parameter '{}' twice",
                        param.name
                    ),
                );
            }
            let kind = param.kind_or_default();
            if !KNOWN_PARAM_TYPES.contains(&kind) {
                report.error(
                    p,
                    format!(
                        "transition '{tname}' parameter '{}' has type '{kind}'; use {}",
                        param.name,
                        KNOWN_PARAM_TYPES.join(", ")
                    ),
                );
            }
            if let Some(items) = &param.items {
                if kind != "array" {
                    report.error(
                        p,
                        format!(
                            "transition '{tname}' parameter '{}' sets items but is not an array",
                            param.name
                        ),
                    );
                } else if !KNOWN_PARAM_ITEMS.contains(&items.as_str()) {
                    report.error(
                        p,
                        format!(
                            "transition '{tname}' parameter '{}' has items '{items}'; use {}",
                            param.name,
                            KNOWN_PARAM_ITEMS.join(", ")
                        ),
                    );
                }
            }
        }
    }
}

/// Every edge out of `phase`, labelled.
fn edges(phase: &PhaseDefinition) -> Vec<(&'static str, &str)> {
    let mut out: Vec<(&'static str, &str)> = Vec::new();
    if let Some(n) = &phase.next {
        out.push(("next", n));
    }
    if let Some(n) = &phase.on_reject {
        out.push(("on_reject", n));
    }
    if let Some(n) = &phase.on_error {
        out.push(("on_error", n));
    }
    for peer in &phase.parallel_with {
        out.push(("parallel_with", peer));
    }
    for t in &phase.transitions {
        if let Some(n) = &t.next {
            out.push(("transition", n));
        }
    }
    if let Some(n) = &phase.skip_to {
        out.push(("skip_to", n));
    }
    out
}

fn check_reachability(def: &WorkflowDefinition, report: &mut ValidationReport) {
    let by_name: BTreeMap<&str, &PhaseDefinition> =
        def.phases.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut reached: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = vec![def.phases[0].name.as_str()];
    while let Some(name) = stack.pop() {
        if !reached.insert(name) {
            continue;
        }
        if let Some(phase) = by_name.get(name) {
            for (_, target) in edges(phase) {
                stack.push(target);
            }
        }
    }
    for phase in &def.phases {
        if !reached.contains(phase.name.as_str()) {
            report.warn(
                Some(&phase.name),
                format!(
                    "phase is unreachable from the first phase '{}'",
                    def.phases[0].name
                ),
            );
        }
    }
}

/// A cycle made only of unconditional `next` edges never terminates unless
/// something on it is bounded.
fn check_unbounded_cycles(def: &WorkflowDefinition, report: &mut ValidationReport) {
    if def.max_total_phase_runs.unwrap_or(0) > 0 {
        return;
    }
    let by_name: BTreeMap<&str, &PhaseDefinition> =
        def.phases.iter().map(|p| (p.name.as_str(), p)).collect();
    // Each cycle is reported once, keyed by its sorted membership.
    let mut reported: BTreeSet<Vec<&str>> = BTreeSet::new();
    for start in &def.phases {
        let mut path: Vec<&str> = vec![start.name.as_str()];
        let mut current = start;
        loop {
            // A phase that decides its own exit is not an unconditional edge.
            if !current.transitions.is_empty() || current.is_human() {
                break;
            }
            let Some(next) = current.next.as_deref() else {
                break;
            };
            if next == start.name {
                let bounded = path.iter().any(|n| {
                    by_name
                        .get(n)
                        .is_some_and(|p| p.max_iterations_or_unlimited() > 0)
                });
                let mut members = path.clone();
                members.sort_unstable();
                if !bounded && reported.insert(members) {
                    report.error(
                        Some(&start.name),
                        format!(
                            "the next-edges {} -> {} form a cycle that never ends; set \
                             max_iterations on one of these phases or max_total_phase_runs",
                            path.join(" -> "),
                            start.name
                        ),
                    );
                }
                break;
            }
            if path.contains(&next) {
                break;
            }
            let Some(phase) = by_name.get(next) else {
                break;
            };
            path.push(next);
            current = phase;
        }
    }
}

/// `skip_to` edges that lead back to where they started would skip forever
/// once their guards all hold.
fn check_skip_cycles(def: &WorkflowDefinition, report: &mut ValidationReport) {
    let by_name: BTreeMap<&str, &PhaseDefinition> =
        def.phases.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut reported: BTreeSet<Vec<&str>> = BTreeSet::new();
    for start in &def.phases {
        let mut path: Vec<&str> = vec![start.name.as_str()];
        let mut current = start;
        while current.skip_when.is_some() {
            let Some(next) = current.skip_to.as_deref() else {
                break;
            };
            if next == start.name {
                let mut members = path.clone();
                members.sort_unstable();
                if reported.insert(members) {
                    report.error(
                        Some(&start.name),
                        format!(
                            "the skip_to edges {} -> {} form a cycle that skips forever once \
                             their guards hold",
                            path.join(" -> "),
                            start.name
                        ),
                    );
                }
                break;
            }
            if path.contains(&next) {
                break;
            }
            let Some(phase) = by_name.get(next) else {
                break;
            };
            path.push(next);
            current = phase;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TransitionDefinition, TransitionParam, WorkflowMode};

    fn phase(name: &str, next: Option<&str>) -> PhaseDefinition {
        PhaseDefinition {
            name: name.into(),
            next: next.map(str::to_string),
            ..Default::default()
        }
    }

    fn phased(phases: Vec<PhaseDefinition>) -> WorkflowDefinition {
        WorkflowDefinition {
            mode: Some(WorkflowMode::Phased),
            phases,
            ..Default::default()
        }
    }

    #[test]
    fn a_linear_workflow_is_valid() {
        let def = phased(vec![phase("plan", Some("execute")), phase("execute", None)]);
        let report = validate_definition("linear", &def);
        assert!(report.is_ok(), "{}", report.render());
        assert!(report.diagnostics.is_empty(), "{}", report.render());
    }

    #[test]
    fn a_bad_workflow_name_is_an_error() {
        let def = phased(vec![phase("plan", None)]);
        assert!(!validate_definition("Release-Review", &def).is_ok());
    }

    #[test]
    fn an_empty_phased_workflow_is_an_error() {
        let report = validate_definition("empty", &phased(vec![]));
        assert!(report.errors()[0].message.contains("at least one"));
    }

    #[test]
    fn a_non_phased_definition_skips_graph_checks() {
        let def = WorkflowDefinition {
            mode: Some(WorkflowMode::Standard),
            phases: vec![phase("plan", Some("nowhere"))],
            ..Default::default()
        };
        assert!(validate_definition("descriptive", &def).is_ok());
    }

    #[test]
    fn duplicate_and_dangling_names_are_errors() {
        let def = phased(vec![phase("plan", Some("ghost")), phase("plan", None)]);
        let report = validate_definition("dupes", &def);
        let text = report.render();
        assert!(text.contains("declared more than once"), "{text}");
        assert!(text.contains("next = 'ghost'"), "{text}");
    }

    #[test]
    fn unbounded_next_cycle_is_an_error_but_a_bounded_one_is_not() {
        let def = phased(vec![phase("a", Some("b")), phase("b", Some("a"))]);
        let report = validate_definition("loop", &def);
        assert_eq!(report.errors().len(), 1, "{}", report.render());
        assert!(report.render().contains("never ends"));

        let mut bounded = def.clone();
        bounded.phases[1].max_iterations = Some(3);
        assert!(validate_definition("loop", &bounded).is_ok());

        let mut capped = def;
        capped.max_total_phase_runs = Some(10);
        assert!(validate_definition("loop", &capped).is_ok());
    }

    #[test]
    fn an_on_reject_loop_through_review_is_valid() {
        let mut review = phase("review", Some("done"));
        review.require_explicit_review = true;
        review.on_reject = Some("execute".into());
        let def = phased(vec![
            phase("execute", Some("review")),
            review,
            phase("done", None),
        ]);
        let report = validate_definition("review_loop", &def);
        assert!(report.is_ok(), "{}", report.render());
    }

    #[test]
    fn on_reject_without_a_way_to_reject_is_a_warning() {
        let mut p = phase("execute", None);
        p.on_reject = Some("execute".into());
        let report = validate_definition("w", &phased(vec![p]));
        assert!(report.is_ok());
        assert!(
            report.warnings()[0]
                .message
                .contains("nothing in this phase can reject")
        );
    }

    #[test]
    fn post_init_invariants_match_agenthicc() {
        let mut p = phase("serve", None);
        p.command_lifecycle = Some("service".into());
        let report = validate_definition("svc", &phased(vec![p.clone()]));
        assert!(report.render().contains("requires terminal_wait_policy"));

        p.terminal_wait_policy = Some("background".into());
        assert!(validate_definition("svc", &phased(vec![p.clone()])).is_ok());

        let mut q = phase("ready", None);
        q.require_readiness = true;
        assert!(
            validate_definition("svc", &phased(vec![q]))
                .render()
                .contains("require_readiness requires")
        );
    }

    #[test]
    fn unknown_enums_are_errors() {
        let mut p = phase("x", None);
        p.output_schema = Some("yaml".into());
        p.mode_override = Some("turbo".into());
        p.allowed_capabilities = Some(vec!["teleport".into()]);
        p.extra_tools = vec!["nope".into()];
        let report = validate_definition("enums", &phased(vec![p]));
        assert_eq!(report.errors().len(), 4, "{}", report.render());
    }

    #[test]
    fn unreachable_phase_is_a_warning() {
        let def = phased(vec![phase("a", None), phase("orphan", None)]);
        let report = validate_definition("w", &def);
        assert!(report.is_ok());
        assert!(report.render().contains("unreachable"));
    }

    #[test]
    fn transitions_are_checked() {
        let mut p = phase("design", None);
        p.transitions = vec![
            TransitionDefinition {
                tool: "submit_design".into(),
                description: "Submit the design.".into(),
                params: vec![TransitionParam {
                    name: "design".into(),
                    ..Default::default()
                }],
                next: Some("build".into()),
                ..Default::default()
            },
            TransitionDefinition {
                tool: "approve_review".into(),
                description: "clash".into(),
                ..Default::default()
            },
        ];
        let def = phased(vec![p, phase("build", None)]);
        let report = validate_definition("t", &def);
        assert_eq!(report.errors().len(), 1, "{}", report.render());
        assert!(report.render().contains("collides with a built-in"));
    }

    #[test]
    fn transition_targets_and_guards_are_validated() {
        let mut p = phase("work", None);
        p.transitions = vec![TransitionDefinition {
            tool: "finish".into(),
            description: "done".into(),
            next: Some("ghost".into()),
            guard: Some("moon_is_full".into()),
            outcome: Some("maybe".into()),
            ..Default::default()
        }];
        let report = validate_definition("t", &phased(vec![p]));
        assert_eq!(report.errors().len(), 3, "{}", report.render());
    }

    #[test]
    fn phase_model_params_must_name_phases() {
        let mut def = phased(vec![phase("plan", None)]);
        def.params
            .insert("plan_model".into(), toml::Value::String("m".into()));
        def.params
            .insert("ghost_model".into(), toml::Value::String("m".into()));
        let report = validate_definition("p", &def);
        assert!(report.is_ok());
        assert_eq!(report.warnings().len(), 1, "{}", report.render());
    }

    #[test]
    fn snake_identifier_rules() {
        assert!(is_snake_identifier("code_plan"));
        assert!(is_snake_identifier("a1"));
        assert!(!is_snake_identifier("1a"));
        assert!(!is_snake_identifier("CodePlan"));
        assert!(!is_snake_identifier("code-plan"));
        assert!(!is_snake_identifier(""));
    }

    #[test]
    fn max_attempts_must_be_positive() {
        let mut p = phase("x", None);
        p.max_attempts = Some(0);
        assert!(
            validate_definition("a", &phased(vec![p.clone()]))
                .render()
                .contains("max_attempts must be at least 1")
        );
        p.max_attempts = Some(5);
        assert!(
            validate_definition("a", &phased(vec![p]))
                .diagnostics
                .is_empty()
        );
    }

    #[test]
    fn skip_when_needs_a_known_guard_and_a_real_target() {
        let mut work = phase("work", Some("work"));
        work.extra_tools = vec!["goal_list".into()];
        work.transitions = vec![TransitionDefinition {
            tool: "done_one".into(),
            description: "d".into(),
            ..Default::default()
        }];
        work.skip_when = Some("goals_complete".into());
        work.skip_to = Some("report".into());
        let def = phased(vec![work.clone(), phase("report", None)]);
        let report = validate_definition("loop", &def);
        assert!(report.diagnostics.is_empty(), "{}", report.render());

        let mut bad = work.clone();
        bad.skip_when = Some("moon_is_full".into());
        bad.skip_to = None;
        let text = validate_definition("loop", &phased(vec![bad, phase("report", None)])).render();
        assert!(
            text.contains("skip_when guard 'moon_is_full' is unknown"),
            "{text}"
        );
        assert!(text.contains("skip_when needs skip_to"), "{text}");

        let mut own = work.clone();
        own.skip_to = Some("work".into());
        assert!(
            validate_definition("loop", &phased(vec![own]))
                .render()
                .contains("must not name the phase itself")
        );

        let mut orphan_target = phase("a", None);
        orphan_target.skip_to = Some("a".into());
        assert!(
            validate_definition("s", &phased(vec![orphan_target]))
                .render()
                .contains("only valid together with skip_when")
        );
    }

    #[test]
    fn skip_to_makes_its_target_reachable_and_skip_cycles_are_errors() {
        let mut a = phase("a", Some("a"));
        a.max_iterations = Some(3);
        a.skip_when = Some("has_artifacts".into());
        a.skip_to = Some("b".into());
        let mut b = phase("b", None);
        b.skip_when = Some("has_artifacts".into());
        b.skip_to = Some("a".into());
        let report = validate_definition("skips", &phased(vec![a, b]));
        assert!(
            !report.render().contains("unreachable"),
            "{}",
            report.render()
        );
        assert!(
            report.render().contains("form a cycle that skips forever"),
            "{}",
            report.render()
        );
    }

    #[test]
    fn transition_parameters_may_declare_json_types() {
        let mut p = phase("verify", None);
        p.transitions = vec![TransitionDefinition {
            tool: "verdict".into(),
            description: "d".into(),
            params: vec![
                TransitionParam {
                    name: "satisfied".into(),
                    kind: Some("boolean".into()),
                    ..Default::default()
                },
                TransitionParam {
                    name: "files".into(),
                    kind: Some("array".into()),
                    ..Default::default()
                },
                TransitionParam {
                    name: "params".into(),
                    kind: Some("array".into()),
                    items: Some("object".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let report = validate_definition("typed", &phased(vec![p.clone()]));
        assert!(report.diagnostics.is_empty(), "{}", report.render());

        p.transitions[0].params[0].kind = Some("float".into());
        p.transitions[0].params[1].items = Some("number".into());
        p.transitions[0].params.push(TransitionParam {
            name: "note".into(),
            items: Some("string".into()),
            ..Default::default()
        });
        let text = validate_definition("typed", &phased(vec![p])).render();
        assert!(text.contains("has type 'float'"), "{text}");
        assert!(text.contains("has items 'number'"), "{text}");
        assert!(text.contains("sets items but is not an array"), "{text}");
    }

    #[test]
    fn a_model_key_names_a_model_param_without_a_warning() {
        let mut implement = phase("implement_goal", None);
        implement.model_key = Some("implement_model".into());
        let mut def = phased(vec![implement]);
        def.params
            .insert("implement_model".into(), toml::Value::String("m".into()));
        let report = validate_definition("goals", &def);
        assert!(report.diagnostics.is_empty(), "{}", report.render());
        assert_eq!(
            def.model_for_phase(&def.phases[0]).as_deref(),
            Some("m"),
            "the aliased key selects the model"
        );

        def.params
            .insert("implement_model".into(), toml::Value::Integer(3));
        assert!(!validate_definition("goals", &def).is_ok());
        def.phases[0].model_key = Some("Not-Snake".into());
        assert!(
            validate_definition("goals", &def)
                .render()
                .contains("model_key 'Not-Snake'")
        );
    }

    #[test]
    fn a_goal_guard_only_needs_a_goal_list_somewhere_in_the_workflow() {
        let mut work = phase("work", Some("finish"));
        work.extra_tools = vec!["goal_list".into()];
        let mut finish = phase("finish", None);
        finish.transitions = vec![TransitionDefinition {
            tool: "complete".into(),
            description: "d".into(),
            guard: Some("goals_complete".into()),
            ..Default::default()
        }];
        let report = validate_definition("g", &phased(vec![work, finish.clone()]));
        assert!(report.diagnostics.is_empty(), "{}", report.render());

        let lonely = validate_definition("g", &phased(vec![finish]));
        assert!(
            lonely.render().contains("no goal is ever recorded"),
            "{}",
            lonely.render()
        );
    }

    #[test]
    fn script_phases_need_a_command_and_only_script_phases_take_one() {
        let mut script = phase("check", None);
        script.agent_type = "script".into();
        let report = validate_definition("s", &phased(vec![script.clone()]));
        assert!(report.render().contains("needs a non-empty `command`"));

        script.command = Some("cargo test".into());
        script.on_reject = Some("check".into());
        script.max_iterations = Some(3);
        let report = validate_definition("s", &phased(vec![script]));
        assert!(report.is_ok(), "{}", report.render());
        assert!(
            !report.render().contains("nothing in this phase can reject"),
            "a failing command rejects"
        );

        let mut agent = phase("x", None);
        agent.command = Some("ls".into());
        assert!(
            validate_definition("s", &phased(vec![agent]))
                .render()
                .contains("only valid with agent_type")
        );
    }
}
