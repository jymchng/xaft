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
    ("goal_flow", include_str!("builtin/goal_flow.toml")),
    ("make_book", include_str!("builtin/make_book.toml")),
    (
        "make_xaft_tool",
        include_str!("builtin/make_xaft_tool.toml"),
    ),
    (
        "parallel_code_plan",
        include_str!("builtin/parallel_code_plan.toml"),
    ),
    (
        "reconstruct_site",
        include_str!("builtin/reconstruct_site.toml"),
    ),
    ("site_imitate", include_str!("builtin/site_imitate.toml")),
    ("ui_ux_pro_max", include_str!("builtin/ui_ux_pro_max.toml")),
];

/// `(alias, canonical name)` pairs: names a built-in also answers to.
///
/// An alias is resolvable (`xaft workflows run make_agenthicc_tool`,
/// `/workflow make_agenthicc_tool`) but never listed, and any workflow
/// actually named like the alias shadows it.
pub const BUILTIN_ALIASES: &[(&str, &str)] = &[("make_agenthicc_tool", "make_xaft_tool")];

/// The canonical name an alias stands for.
pub fn builtin_alias(name: &str) -> Option<&'static str> {
    BUILTIN_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, canonical)| *canonical)
}

/// The TOML source of a built-in phased workflow (aliases resolve).
pub fn builtin_source(name: &str) -> Option<&'static str> {
    let name = builtin_alias(name).unwrap_or(name);
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
        assert_eq!(plan.params[1].json_type(), ("array", Some("string")));
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
    fn ui_ux_pro_max_declares_every_typed_phase_tool() {
        let def = builtin("ui_ux_pro_max");
        assert_eq!(def.phases.len(), 12);
        assert_eq!(def.max_total_phase_runs, Some(64));
        for p in &def.phases {
            assert_eq!(p.max_attempts, Some(1), "{}: one turn per phase", p.name);
            assert_eq!(p.max_turns_param.as_deref(), Some("max_phase_turns"));
            assert_eq!(p.max_turns, Some(24));
            assert!(
                p.mode_override.is_none(),
                "{} follows the session mode",
                p.name
            );
            assert!(
                p.transitions
                    .iter()
                    .all(|t| t.effect.as_deref() == Some("ui_ux_pro_max")),
                "{}",
                p.name
            );
        }
        let discovery = transition(&def, "product_discovery", "complete_product_discovery");
        assert_eq!(discovery.params.len(), 17);
        let criteria = discovery
            .params
            .iter()
            .find(|p| p.name == "acceptance_criteria")
            .unwrap();
        assert!(criteria.required && criteria.json_type() == ("array", Some("string")));
        let review = phase(&def, "design_system_review");
        let tools: Vec<&str> = review.transitions.iter().map(|t| t.tool.as_str()).collect();
        assert_eq!(
            tools,
            [
                "approve_design_system",
                "request_design_system_revision",
                "block_design_system"
            ]
        );
        assert_eq!(review.on_reject.as_deref(), Some("design_system_synthesis"));
        assert_eq!(
            transition(
                &def,
                "experience_architecture",
                "submit_experience_architecture"
            )
            .guard
            .as_deref(),
            Some("implementation_plan_approved")
        );
        assert_eq!(
            phase(&def, "experience_architecture").extra_tools,
            ["ui_ux_pro_max"]
        );
        let mut tuned = def.clone();
        tuned
            .params
            .insert("max_phase_turns".into(), toml::Value::Integer(7));
        assert_eq!(tuned.max_turns_for_phase(&tuned.phases[0]), 7);
        assert!(
            validate_definition("ui_ux_pro_max", &tuned)
                .diagnostics
                .is_empty()
        );
    }

    #[test]
    fn reconstruct_site_follows_the_authoritative_plan() {
        let def = builtin("reconstruct_site");
        let order = names(&def);
        assert_eq!(order.len(), 41);
        assert_eq!(order.first(), Some(&"init"));
        assert_eq!(order.last(), Some(&"final_validation"));
        let i = |n: &str| order.iter().position(|p| *p == n).unwrap();
        assert!(i("research_gate") < i("bootstrap"));
        assert!(i("fidelity_pass") < i("sqlite_db"));
        for p in &def.phases {
            assert_eq!(p.extra_tools, ["reconstruct"], "{}", p.name);
            assert_eq!(p.mode_override.as_deref(), Some("yolo"));
            assert!(
                p.transitions
                    .iter()
                    .all(|t| t.effect.as_deref() == Some("reconstruct_site")),
                "{}",
                p.name
            );
            if p.name.starts_with("verify_") {
                let verified = p
                    .transitions
                    .iter()
                    .find(|t| t.tool.ends_with("_verified"))
                    .unwrap();
                assert_eq!(verified.guard.as_deref(), Some("infra_artifacts_present"));
                assert_eq!(
                    p.on_reject.as_deref(),
                    Some(p.name.as_str()),
                    "rejections retry"
                );
            }
        }
        let gate = phase(&def, "research_gate");
        let tools: Vec<&str> = gate.transitions.iter().map(|t| t.tool.as_str()).collect();
        assert_eq!(
            tools,
            [
                "approve_research_baseline",
                "approve_degraded_research",
                "reject_research_baseline"
            ]
        );
        assert_eq!(
            transition(&def, "final_validation", "final_blocked").outcome_or_default(),
            "exit"
        );
        assert!(transition(&def, "final_validation", "final_approved").end);
        let rejected = transition(&def, "visual_validation", "visual_rejected");
        assert_eq!(rejected.params[0].json_type(), ("array", Some("object")));
        assert!(
            !rejected.params[1].required,
            "an empty target retries the phase"
        );
        let init = transition(&def, "init", "submit_initial_state");
        assert_eq!(init.params.len(), 13);
        assert_eq!(
            init.params
                .iter()
                .find(|p| p.name == "reference_is_static")
                .unwrap()
                .kind_or_default(),
            "boolean"
        );
    }

    #[test]
    fn reconstruct_site_reads_agenthiccs_model_knob_aliases() {
        let mut def = builtin("reconstruct_site");
        for (knob, phase_name) in [
            ("visual_model", "visual_research"),
            ("interaction_model", "interaction_analysis"),
            ("assets_model", "content_assets"),
            ("design_model", "design_system"),
            ("shell_model", "global_shell"),
            ("components_model", "component_system"),
            ("data_model", "data_layer"),
            ("responsive_model", "responsive_pass"),
            ("fidelity_model", "fidelity_pass"),
            ("final_model", "final_validation"),
            ("docker_model", "docker"),
            ("research_gate_model", "research_gate"),
        ] {
            def.params
                .insert(knob.into(), toml::Value::String(format!("m-{knob}")));
            assert_eq!(
                def.model_for_phase(phase(&def, phase_name)).as_deref(),
                Some(format!("m-{knob}").as_str()),
                "{knob}"
            );
        }
        let report = validate_definition("reconstruct_site", &def);
        assert!(report.diagnostics.is_empty(), "{}", report.render());
    }

    #[test]
    fn goal_flow_matches_agenthicc_topology_and_transitions() {
        let def = builtin("goal_flow");
        assert_eq!(
            names(&def),
            [
                "clarify",
                "decide_goals",
                "implement_goal",
                "verify_goal",
                "summarize"
            ]
        );
        assert!(def.mode_bindings.is_empty(), "manual only, as agenthicc");
        let implement = &def.phases[2];
        assert_eq!(implement.next.as_deref(), Some("verify_goal"));
        assert_eq!(implement.skip_when.as_deref(), Some("goals_complete"));
        assert_eq!(implement.skip_to.as_deref(), Some("summarize"));
        assert_eq!(implement.on_reject.as_deref(), Some("implement_goal"));
        assert_eq!(implement.mode_override.as_deref(), Some("yolo"));
        assert_eq!(implement.model_param.as_deref(), Some("implement_model"));
        assert_eq!(def.phases[3].model_param.as_deref(), Some("verify_model"));
        assert_eq!(def.phases[3].next.as_deref(), Some("implement_goal"));
        for phase in &def.phases {
            assert_eq!(phase.max_attempts, Some(5), "agenthicc's _MAX_ATTEMPTS");
        }

        let notes = transition(&def, "clarify", "complete_clarification");
        assert_eq!(notes.params[0].name, "notes");
        let goals = transition(&def, "decide_goals", "finalize_goals");
        assert_eq!(goals.effect.as_deref(), Some("finalize_goals"));
        assert_eq!(goals.params[0].kind_or_default(), "array");
        let done = transition(&def, "implement_goal", "goal_implemented");
        assert_eq!(done.effect.as_deref(), Some("goal_implemented"));
        let names: Vec<&str> = done.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["summary", "files"]);
        let verdict = transition(&def, "verify_goal", "verify_goal");
        assert_eq!(verdict.effect.as_deref(), Some("goal_verdict"));
        assert_eq!(verdict.params[0].name, "satisfied");
        assert_eq!(verdict.params[0].kind_or_default(), "boolean");
        let complete = transition(&def, "summarize", "complete_workflow");
        assert_eq!(complete.guard.as_deref(), Some("goals_complete"));
        assert!(complete.end);
    }

    #[test]
    fn goal_flow_reads_agenthiccs_phase_model_params() {
        let mut def = builtin("goal_flow");
        def.params
            .insert("implement_model".into(), toml::Value::String("fast".into()));
        def.params
            .insert("clarify_model".into(), toml::Value::String("smart".into()));
        let report = validate_definition("goal_flow", &def);
        assert!(report.diagnostics.is_empty(), "{}", report.render());
        assert_eq!(def.model_for_phase(&def.phases[2]).as_deref(), Some("fast"));
        assert_eq!(
            def.model_for_phase(&def.phases[0]).as_deref(),
            Some("smart")
        );
        assert_eq!(def.model_for_phase(&def.phases[3]), None);
    }

    #[test]
    fn parallel_code_plan_matches_agenthicc_topology() {
        let def = builtin("parallel_code_plan");
        assert_eq!(
            names(&def),
            [
                "plan",
                "decompose",
                "dispatch",
                "collect",
                "integrate",
                "verify",
                "summarize"
            ]
        );
        assert_eq!(def.mode_bindings, ["plan"]);
        assert!(def.phases[0].require_plan_finalization);
        for phase in &def.phases[1..6] {
            assert_eq!(phase.extra_tools, ["parallel_workers"], "{}", phase.name);
        }
        for phase in &def.phases[..6] {
            assert_eq!(
                phase.max_attempts,
                Some(1),
                "agenthicc gives each phase one turn"
            );
        }
        assert_eq!(
            def.phases[5].allowed_capabilities.as_deref(),
            Some(
                &[
                    "read".to_string(),
                    "search".into(),
                    "git_read".into(),
                    "execute".into()
                ][..]
            )
        );
    }

    #[test]
    fn make_xaft_tool_publishes_only_through_a_guarded_transition() {
        let def = builtin("make_xaft_tool");
        assert_eq!(names(&def), ["analyze", "generate", "validate", "finalize"]);
        let plan = transition(&def, "analyze", "submit_tool_plan");
        assert_eq!(plan.effect.as_deref(), Some("record_tool_plan"));
        let draft = transition(&def, "generate", "confirm_generation_complete");
        assert_eq!(draft.effect.as_deref(), Some("record_tool_draft"));
        let approve = transition(&def, "validate", "approve_tool");
        assert_eq!(approve.guard.as_deref(), Some("tool_draft_valid"));
        assert_eq!(approve.effect.as_deref(), Some("publish_custom_tool"));
        let reject = transition(&def, "validate", "reject_tool");
        assert_eq!(reject.outcome_or_default(), "rejected");
        assert_eq!(def.phases[2].on_reject.as_deref(), Some("generate"));
    }

    #[test]
    fn make_agenthicc_tool_is_an_alias_of_make_xaft_tool() {
        assert_eq!(builtin_alias("make_agenthicc_tool"), Some("make_xaft_tool"));
        assert_eq!(
            builtin_source("make_agenthicc_tool"),
            builtin_source("make_xaft_tool")
        );
        assert!(
            BUILTIN_PHASED_SOURCES
                .iter()
                .all(|(n, _)| builtin_alias(n).is_none()),
            "an alias is never also a listed name"
        );
    }

    #[test]
    fn make_book_matches_agenthicc_topology_and_gates() {
        let def = builtin("make_book");
        assert_eq!(
            names(&def),
            [
                "toc",
                "research",
                "assets",
                "chapter",
                "layout_review",
                "front_matter",
                "back_matter",
                "final_layout_review",
                "compile"
            ]
        );
        let chapter = &def.phases[3];
        assert_eq!(
            chapter.next.as_deref(),
            Some("chapter"),
            "one entry per chapter"
        );
        assert_eq!(chapter.skip_when.as_deref(), Some("chapters_complete"));
        assert_eq!(chapter.skip_to.as_deref(), Some("layout_review"));
        for (phase, tool, effect) in [
            ("toc", "submit_toc", "book_submit_toc"),
            ("research", "submit_research", "book_submit_research"),
            ("assets", "confirm_assets_ready", "book_confirm_assets"),
            (
                "chapter",
                "confirm_chapter_complete",
                "book_confirm_chapter",
            ),
            (
                "layout_review",
                "confirm_layout_ready",
                "book_confirm_layout",
            ),
            (
                "front_matter",
                "confirm_front_matter_ready",
                "book_confirm_front_matter",
            ),
            (
                "back_matter",
                "confirm_back_matter_ready",
                "book_confirm_back_matter",
            ),
            (
                "final_layout_review",
                "confirm_layout_ready",
                "book_confirm_final_layout",
            ),
            ("compile", "mark_book_complete", "book_mark_complete"),
        ] {
            let t = transition(&def, phase, tool);
            assert_eq!(t.effect.as_deref(), Some(effect), "{phase}.{tool}");
            assert_eq!(t.params.len(), 1, "agenthicc's gates take only a summary");
        }
        assert_eq!(
            transition(&def, "compile", "reject_book").outcome_or_default(),
            "exit"
        );
        assert_eq!(def.phases[8].extra_tools, ["book"]);
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
