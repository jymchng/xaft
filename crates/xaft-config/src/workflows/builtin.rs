//! Built-in phased workflows, compiled into the binary.
//!
//! agenthicc ships its built-in workflows as Python packages registered lazily
//! from `_BUILTIN_WORKFLOW_DESCRIPTORS`. xaft ships each one as a TOML
//! definition embedded with `include_str!`, parsed on demand, and executed by
//! the same phased engine that runs user workflows — so a built-in is also a
//! worked example of everything a custom workflow can declare
//! (`xaft workflows show code_plan --toml`).

use crate::types::WorkflowDefinition;

use super::discovery::parse_workflow_toml;

/// `(name, TOML source)` for every built-in phased workflow, in listing order.
pub const BUILTIN_PHASED_SOURCES: &[(&str, &str)] = &[
    ("code_plan", include_str!("builtin/code_plan.toml")),
    (
        "create_workflow",
        include_str!("builtin/create_workflow.toml"),
    ),
];

/// The TOML source of a built-in phased workflow.
pub fn builtin_source(name: &str) -> Option<&'static str> {
    BUILTIN_PHASED_SOURCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, src)| *src)
}

/// Parse every built-in phased workflow.
///
/// # Panics
///
/// Panics when an embedded definition does not parse. That is a build defect,
/// not a runtime condition: `every_builtin_parses_and_validates` fails first.
pub fn builtin_phased_definitions() -> Vec<(String, WorkflowDefinition)> {
    BUILTIN_PHASED_SOURCES
        .iter()
        .map(|(name, src)| {
            let def = parse_workflow_toml(src)
                .unwrap_or_else(|e| panic!("built-in workflow '{name}' does not parse: {e}"));
            ((*name).to_string(), def)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::validate::validate_definition;

    #[test]
    fn every_builtin_parses_and_validates() {
        for (name, def) in builtin_phased_definitions() {
            assert_eq!(
                def.name.as_deref(),
                Some(name.as_str()),
                "embedded name must match its registry key"
            );
            assert!(def.is_phased(), "{name} must be phased");
            let report = validate_definition(&name, &def);
            assert!(
                report.diagnostics.is_empty(),
                "built-in '{name}' must validate without findings:\n{}",
                report.render()
            );
        }
    }

    #[test]
    fn code_plan_matches_agenthicc_topology() {
        let def = builtin_phased_definitions()
            .into_iter()
            .find(|(n, _)| n == "code_plan")
            .unwrap()
            .1;
        let names: Vec<&str> = def.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["plan", "execute", "review", "summarize"]);
        assert!(def.phases[0].require_plan_finalization);
        assert!(def.phases[1].require_explicit_completion);
        assert!(def.phases[2].require_explicit_review);
        assert_eq!(def.phases[2].on_reject.as_deref(), Some("execute"));
        assert_eq!(def.mode_bindings, ["plan"]);
    }

    #[test]
    fn create_workflow_publishes_only_through_a_guarded_transition() {
        let def = builtin_phased_definitions()
            .into_iter()
            .find(|(n, _)| n == "create_workflow")
            .unwrap()
            .1;
        let validate = def.phases.iter().find(|p| p.name == "validate").unwrap();
        let approve = validate
            .transitions
            .iter()
            .find(|t| t.tool == "approve_workflow")
            .unwrap();
        assert_eq!(approve.guard.as_deref(), Some("draft_valid"));
        assert_eq!(approve.effect.as_deref(), Some("publish_workflow_draft"));
        let reject = validate
            .transitions
            .iter()
            .find(|t| t.tool == "reject_workflow")
            .unwrap();
        assert_eq!(reject.outcome_or_default(), "rejected");
        assert_eq!(validate.on_reject.as_deref(), Some("generate"));
    }
}
