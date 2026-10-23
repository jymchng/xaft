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
    ("copy_website", include_str!("builtin/copy_website.toml")),
    ("site_imitate", include_str!("builtin/site_imitate.toml")),
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

    fn builtin(name: &str) -> WorkflowDefinition {
        builtin_phased_definitions()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no built-in '{name}'"))
            .1
    }

    fn phase<'a>(def: &'a WorkflowDefinition, name: &str) -> &'a crate::types::PhaseDefinition {
        def.phases.iter().find(|p| p.name == name).unwrap()
    }

    fn transition<'a>(
        def: &'a WorkflowDefinition,
        phase_name: &str,
        tool: &str,
    ) -> &'a crate::types::TransitionDefinition {
        phase(def, phase_name)
            .transitions
            .iter()
            .find(|t| t.tool == tool)
            .unwrap_or_else(|| panic!("{phase_name} has no {tool}"))
    }

    fn names(def: &WorkflowDefinition) -> Vec<&str> {
        def.phases.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn copy_website_matches_agenthicc_topology() {
        let def = builtin("copy_website");
        assert_eq!(
            names(&def),
            [
                "extract_target",
                "site_study",
                "design_spec",
                "scaffold",
                "implement_layout",
                "implement_pages",
                "implement_data",
                "responsive_pass",
                "parity_verify",
                "final_report"
            ]
        );
        assert!(def.mode_bindings.is_empty(), "manual only");
        assert!(
            def.stable_system_prompt
                .contains("CACHE-STABLE WORKFLOW POLICY")
        );
        for p in &def.phases[..9] {
            assert_eq!(
                p.max_attempts,
                Some(5),
                "{}: agenthicc's _MAX_ATTEMPTS",
                p.name
            );
            assert_eq!(
                p.transitions.len(),
                if p.name == "parity_verify" { 2 } else { 1 }
            );
        }
        let verify = phase(&def, "parity_verify");
        assert_eq!(verify.on_reject.as_deref(), Some("implement_layout"));
        assert_eq!(verify.max_iterations, Some(5));
        assert_eq!(
            transition(&def, "parity_verify", "reject_parity").outcome_or_default(),
            "rejected"
        );
        assert_eq!(
            phase(&def, "implement_layout").max_iterations,
            Some(5),
            "the runner's _MAX_FIX_ITERATIONS"
        );
        assert!(
            phase(&def, "implement_layout")
                .system_prompt
                .contains("{state.fix_context}")
        );
        assert!(phase(&def, "final_report").transitions.is_empty());
        let target = transition(&def, "extract_target", "submit_target");
        let tool_params: Vec<&str> = target.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(tool_params, ["url", "scope", "pages"]);
    }

    #[test]
    fn copy_website_reads_agenthiccs_grouped_model_knobs() {
        let mut def = builtin("copy_website");
        for (k, v) in [
            ("study_model", "s"),
            ("build_model", "b"),
            ("verify_model", "v"),
        ] {
            def.params.insert(k.into(), toml::Value::String(v.into()));
        }
        let model = |name: &str| def.model_for_phase(phase(&def, name));
        assert_eq!(model("extract_target").as_deref(), Some("s"));
        assert_eq!(model("design_spec").as_deref(), Some("s"));
        assert_eq!(model("implement_pages").as_deref(), Some("b"));
        assert_eq!(model("final_report").as_deref(), Some("b"));
        assert_eq!(model("parity_verify").as_deref(), Some("v"));
        let report = validate_definition("copy_website", &def);
        assert!(report.diagnostics.is_empty(), "{}", report.render());
    }

    #[test]
    fn site_imitate_loops_components_through_its_effect() {
        let def = builtin("site_imitate");
        assert_eq!(
            names(&def),
            [
                "analyze",
                "plan",
                "scaffold",
                "build",
                "verify",
                "final_verify"
            ]
        );
        assert!(
            def.stable_system_prompt
                .contains("MOBILE-FIRST RESPONSIVE SITE CONTRACT")
        );
        assert_eq!(phase(&def, "analyze").extra_tools, ["ui_catalog"]);
        assert_eq!(phase(&def, "build").extra_tools, ["ui_catalog"]);
        let plan = transition(&def, "plan", "submit_plan");
        assert_eq!(plan.params[1].kind_or_default(), "string_list");
        assert_eq!(
            transition(&def, "plan", "request_reanalysis")
                .next
                .as_deref(),
            Some("analyze")
        );
        let verified = transition(&def, "verify", "component_verified");
        assert_eq!(verified.guard.as_deref(), Some("mobile_verified"));
        assert_eq!(verified.effect.as_deref(), Some("site_imitate"));
        assert_eq!(verified.next.as_deref(), Some("final_verify"));
        assert_eq!(
            transition(&def, "verify", "component_verification_failed").outcome_or_default(),
            "rejected"
        );
        assert_eq!(phase(&def, "verify").on_reject.as_deref(), Some("build"));
        assert!(transition(&def, "final_verify", "final_verify_passed").end);
        assert_eq!(
            transition(&def, "final_verify", "final_verify_passed")
                .guard
                .as_deref(),
            Some("mobile_verified")
        );
        assert!(
            transition(&def, "build", "component_built")
                .params
                .is_empty()
        );
        assert!(phase(&def, "analyze").prompt.contains("{intent_head}"));
        // The runner passes no mode to these phases: they follow the session mode.
        for name in ["analyze", "plan", "verify", "final_verify"] {
            assert!(phase(&def, name).mode_override.is_none(), "{name}");
        }
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
