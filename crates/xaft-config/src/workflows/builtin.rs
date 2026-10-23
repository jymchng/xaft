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
            .unwrap_or_else(|| panic!("no built-in {name}"))
            .1
    }

    fn phase_names(def: &WorkflowDefinition) -> Vec<&str> {
        def.phases.iter().map(|p| p.name.as_str()).collect()
    }

    fn transition<'a>(
        def: &'a WorkflowDefinition,
        phase: &str,
        tool: &str,
    ) -> &'a crate::types::TransitionDefinition {
        def.phases
            .iter()
            .find(|p| p.name == phase)
            .and_then(|p| p.transitions.iter().find(|t| t.tool == tool))
            .unwrap_or_else(|| panic!("no transition {tool} in {phase}"))
    }

    #[test]
    fn goal_flow_matches_agenthicc_topology_and_transitions() {
        let def = builtin("goal_flow");
        assert_eq!(
            phase_names(&def),
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
        assert_eq!(implement.model_key.as_deref(), Some("implement_model"));
        assert_eq!(def.phases[3].model_key.as_deref(), Some("verify_model"));
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
            phase_names(&def),
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
        assert_eq!(
            phase_names(&def),
            ["analyze", "generate", "validate", "finalize"]
        );
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
            phase_names(&def),
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
