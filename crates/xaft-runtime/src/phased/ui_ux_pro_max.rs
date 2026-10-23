//! The `ui_ux_pro_max` effect, guard and tool group — the deterministic half
//! of agenthicc's `UiUxProMaxRunner` (`workflows/ui_ux_pro_max/`).
//!
//! agenthicc keeps a typed, bounded `UiUxProMaxContext`; every phase tool
//! validates its submission (`phase_tools.py`) and the runner records it and
//! picks the next state (`runner.py`). Here the context is the run record
//! `ui_ux_pro_max` (same field names, same text and list bounds), and the
//! `ui_ux_pro_max` effect does both halves for whichever phase tool was
//! called:
//!
//! * **routes** — the profile chosen by product discovery (`broad`,
//!   `targeted`, `accessibility_audit`) decides the research phase, whether a
//!   design system is synthesised and whether implementation runs;
//! * **bounded loops** — design-system revisions stop at
//!   `max_design_review_attempts` (1..5, default 2) and then block; audit
//!   findings enter remediation only while `max_remediation_cycles` (0..5,
//!   default 2) remain and the route includes remediation;
//! * **acceptance** — every product-brief criterion must get exactly one
//!   `STATUS | criterion | evidence` result; delivery completes only when no
//!   criterion failed or stayed unverified, and otherwise ends the run early
//!   (agenthicc's terminal `BLOCKED`).
//!
//! After every accepted call the effect renders the next phase's
//! `[DYNAMIC WORKFLOW CONTEXT: …]` JSON block (the runner's `_phase_context`)
//! into `{state.phase_context}`, which the phase prompts use as their task
//! text.
//!
//! The `ui_ux_pro_max` tool group adds `request_implementation_approval(plan)`
//! to the experience-architecture phase; the `implementation_plan_approved`
//! guard keeps `submit_experience_architecture` closed until it was approved
//! in the same phase run.

use std::path::Path;
use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Map, Value, json};

use super::hooks::{
    EffectOutcome, HookContext, Route, arg_list, arg_str, bounded_param, field_list, field_str,
    field_u64, record, save_record, set_state,
};
use super::question::{UserQuestion, UserQuestionGate};
use super::tools::{TransitionEnv, ask_user_approval};

const RECORD: &str = "ui_ux_pro_max";

/// agenthicc's `MAX_TEXT_CHARS`.
pub const MAX_TEXT_CHARS: usize = 4_000;
/// agenthicc's `MAX_LIST_ITEMS`.
pub const MAX_LIST_ITEMS: usize = 40;
/// agenthicc's `MAX_ITEM_CHARS`.
pub const MAX_ITEM_CHARS: usize = 512;

/// One-line description for `list_tool_groups`.
pub const GROUP_SUMMARY: &str = "UI/UX Pro Max plan approval: \
     request_implementation_approval(plan) asks the user to approve the implementation plan \
     (headless runs auto-approve); guard implementation_plan_approved; effect ui_ux_pro_max";

/// agenthicc's `PHASES_BY_PROFILE`.
pub fn phases_for(profile: &str) -> &'static [&'static str] {
    match profile {
        "targeted" => &[
            "product_discovery",
            "targeted_design_research",
            "stack_adaptation",
            "experience_architecture",
            "design_implementation",
            "visual_ux_audit",
            "design_remediation",
            "design_acceptance",
            "delivery",
        ],
        "accessibility_audit" => &[
            "product_discovery",
            "targeted_design_research",
            "stack_adaptation",
            "visual_ux_audit",
            "design_acceptance",
            "delivery",
        ],
        _ => &[
            "product_discovery",
            "design_intelligence",
            "design_system_synthesis",
            "design_system_review",
            "stack_adaptation",
            "experience_architecture",
            "design_implementation",
            "visual_ux_audit",
            "design_remediation",
            "design_acceptance",
            "delivery",
        ],
    }
}

/// agenthicc's `bounded_text`: normalise newlines, drop control characters,
/// trim, and cap the length.
pub fn bounded_text(value: &str, limit: usize) -> String {
    let text = value.replace("\r\n", "\n").replace('\r', "\n");
    let printable: String = text
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    printable.trim().chars().take(limit).collect()
}

/// agenthicc's `bounded_list`.
pub fn bounded_list(items: &[String]) -> Vec<String> {
    items
        .iter()
        .take(MAX_LIST_ITEMS)
        .map(|item| bounded_text(item, MAX_ITEM_CHARS))
        .collect()
}

fn text(ctx: &HookContext<'_>, key: &str) -> String {
    bounded_text(&arg_str(ctx.args, key), MAX_TEXT_CHARS)
}

fn text_n(ctx: &HookContext<'_>, key: &str, limit: usize) -> String {
    bounded_text(&arg_str(ctx.args, key), limit)
}

fn list(ctx: &HookContext<'_>, key: &str) -> Vec<String> {
    bounded_list(&arg_list(ctx.args, key))
}

fn put(rec: &mut Map<String, Value>, key: &str, value: impl Into<Value>) {
    rec.insert(key.to_string(), value.into());
}

fn unique(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    bounded_list(
        &items
            .into_iter()
            .filter(|i| seen.insert(i.clone()))
            .collect::<Vec<_>>(),
    )
}

fn add_artifact(
    rec: &mut Map<String, Value>,
    phase: &str,
    kind: &str,
    summary: &str,
    refs: &[String],
) {
    let mut artifacts = match rec.get("artifacts") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    artifacts.insert(
        phase.to_string(),
        json!({
            "phase": phase,
            "kind": kind,
            "summary": bounded_text(summary, MAX_TEXT_CHARS),
            "references": bounded_list(refs),
        }),
    );
    rec.insert("artifacts".into(), Value::Object(artifacts));
}

/// The configured review budget (agenthicc: 1..5, default 2).
fn max_review_attempts(ctx: &HookContext<'_>) -> u64 {
    bounded_param(ctx.data, "max_design_review_attempts", 2, 1, 5) as u64
}

/// The configured remediation budget (agenthicc: 0..5, default 2).
fn max_remediation_cycles(ctx: &HookContext<'_>) -> u64 {
    bounded_param(ctx.data, "max_remediation_cycles", 2, 0, 5) as u64
}

// ── Skill availability ─────────────────────────────────────────────────────

/// agenthicc's `_seed_skill_availability`: is a UI/UX Pro Max skill in the
/// loaded skill catalog (`.xaft/skills/` or `~/.config/xaft/skills/`)?
fn probe_skill(working_dir: Option<&Path>) -> (bool, String, String) {
    let mut dirs = Vec::new();
    if let Some(wd) = working_dir {
        dirs.push(xaft_skills::SkillLoader::project_skills_dir(wd));
    }
    if let Some(global) = xaft_skills::SkillLoader::global_skills_dir() {
        dirs.push(global);
    }
    let normalise = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_lowercase()
    };
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if stem.starts_with('.') {
                continue;
            }
            let file = if path.is_dir() {
                path.join("SKILL.md")
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                path.clone()
            } else {
                continue;
            };
            if !file.is_file() {
                continue;
            }
            let declared = std::fs::read_to_string(&file)
                .ok()
                .and_then(|body| {
                    body.lines()
                        .skip_while(|l| l.trim() != "---")
                        .skip(1)
                        .take_while(|l| l.trim() != "---")
                        .find_map(|l| {
                            l.trim()
                                .strip_prefix("name:")
                                .map(|n| n.trim().trim_matches('"').to_string())
                        })
                })
                .unwrap_or_default();
            if normalise(&format!("{stem} {declared}")).contains("uiuxpromax") {
                let name = if declared.is_empty() { stem } else { declared };
                return (
                    true,
                    bounded_text(&name, 256),
                    bounded_text(&file.display().to_string(), 1_000),
                );
            }
        }
    }
    (false, String::new(), String::new())
}

fn ensure_skill(ctx: &HookContext<'_>, rec: &mut Map<String, Value>) {
    if rec.contains_key("skill_available") {
        return;
    }
    let (available, name, path) = probe_skill(ctx.data.working_dir());
    put(rec, "skill_available", available);
    put(rec, "skill_name", name);
    put(rec, "skill_path", path);
    put(
        rec,
        "skill_status",
        if available {
            "skill loaded in the current xaft catalog; script and data capabilities remain \
             unverified until inspected"
        } else {
            "skill is not loaded in the current xaft catalog; do not claim its data or search \
             executable is available"
        },
    );
}

// ── Phase context ──────────────────────────────────────────────────────────

fn lst(rec: &Map<String, Value>, key: &str) -> Value {
    json!(field_list(rec, key))
}

fn s(rec: &Map<String, Value>, key: &str) -> Value {
    json!(field_str(rec, key))
}

/// agenthicc's `_phase_context`: the dynamic JSON context of `phase`.
pub fn render_phase_context(
    ctx: &HookContext<'_>,
    rec: &Map<String, Value>,
    phase: &str,
) -> String {
    let mut common = json!({
        "request": bounded_text(ctx.data.intent(), 20_000),
        "project_root": ctx.data.working_dir().map(|p| p.display().to_string()).unwrap_or_default(),
        "scope": field_str(rec, "route_profile"),
        "product_brief": {
            "project": s(rec, "project_name"),
            "product_type": s(rec, "project_type"),
            "platform": s(rec, "platform"),
            "stack": s(rec, "stack"),
            "request_kind": s(rec, "request_kind"),
            "summary": s(rec, "product_summary"),
            "target_users": lst(rec, "target_users"),
            "journeys": lst(rec, "user_journeys"),
            "surfaces": lst(rec, "required_surfaces"),
            "functional_requirements": lst(rec, "functional_requirements"),
            "ux_requirements": lst(rec, "ux_requirements"),
            "accessibility": lst(rec, "accessibility_expectations"),
            "constraints": lst(rec, "technical_constraints"),
            "existing_conventions": lst(rec, "existing_visual_conventions"),
            "acceptance_criteria": lst(rec, "acceptance_criteria"),
            "assumptions": lst(rec, "assumptions"),
        },
        "skill": {
            "available_in_loaded_catalog": rec.get("skill_available").cloned().unwrap_or(Value::Null),
            "name": s(rec, "skill_name"),
            "path": s(rec, "skill_path"),
            "status": s(rec, "skill_status"),
        },
    });
    let concat = |keys: &[&str]| -> Value {
        json!(
            keys.iter()
                .flat_map(|k| field_list(rec, k))
                .collect::<Vec<_>>()
        )
    };
    let phase_data = match phase {
        "design_intelligence" => json!({
            "research_strategy": "broad design-system search",
            "research": s(rec, "research_summary"),
        }),
        "targeted_design_research" => json!({
            "request_kind": s(rec, "request_kind"),
            "research": s(rec, "research_summary"),
        }),
        "design_system_synthesis" => json!({
            "research_summary": s(rec, "research_summary"),
            "findings": lst(rec, "research_findings"),
            "provenance": lst(rec, "research_provenance"),
            "recommendations": lst(rec, "applicable_recommendations"),
            "conflicts": lst(rec, "research_conflicts"),
            "limitations": lst(rec, "research_limitations"),
        }),
        "design_system_review" => json!({
            "design_summary": s(rec, "design_summary"),
            "direction": s(rec, "visual_direction"),
            "style": s(rec, "design_style"),
            "tokens": concat(&["color_tokens", "typography_tokens", "spacing_rules"]),
            "layout": lst(rec, "layout_rules"),
            "components": lst(rec, "component_rules"),
            "accessibility": lst(rec, "accessibility_rules"),
            "anti_patterns": lst(rec, "anti_patterns"),
            "attempt": field_u64(rec, "design_review_attempts") + 1,
            "max_attempts": max_review_attempts(ctx),
        }),
        "stack_adaptation" => json!({
            "approved_design": s(rec, "design_summary"),
            "visual_direction": s(rec, "visual_direction"),
            "design_artifacts": lst(rec, "design_artifact_refs"),
            "detected_stack": s(rec, "stack"),
        }),
        "experience_architecture" => json!({
            "implementation_guide": s(rec, "implementation_guide_summary"),
            "token_mappings": lst(rec, "token_mappings"),
            "component_strategy": lst(rec, "component_strategy"),
            "accessibility_strategy": lst(rec, "accessibility_strategy"),
            "screens": lst(rec, "required_surfaces"),
        }),
        "design_implementation" => json!({
            "design_system": s(rec, "design_summary"),
            "design_references": lst(rec, "design_artifact_refs"),
            "implementation_guide": s(rec, "implementation_guide_summary"),
            "architecture": lst(rec, "page_component_specs"),
            "task_plan": lst(rec, "implementation_plan"),
            "approval_feedback": s(rec, "implementation_approval_feedback"),
            "validation_commands": lst(rec, "validation_commands"),
        }),
        "visual_ux_audit" => json!({
            "changed_files": lst(rec, "changed_files"),
            "implementation_summary": s(rec, "implementation_summary"),
            "implemented_states": lst(rec, "implemented_states"),
            "design_deviations": lst(rec, "design_deviations"),
            "acceptance_criteria": lst(rec, "acceptance_criteria"),
            "prior_findings": lst(rec, "audit_findings"),
            "remediation_cycle": field_u64(rec, "remediation_cycles"),
            "accessibility_audit_only": field_str(rec, "route_profile") == "accessibility_audit",
        }),
        "design_remediation" => json!({
            "audit_summary": s(rec, "audit_summary"),
            "findings": lst(rec, "audit_findings"),
            "cycle": field_u64(rec, "remediation_cycles"),
            "max_cycles": max_remediation_cycles(ctx),
            "already_resolved": lst(rec, "resolved_findings"),
        }),
        "design_acceptance" => json!({
            "requirements": lst(rec, "acceptance_criteria"),
            "audit": s(rec, "audit_summary"),
            "findings": lst(rec, "audit_findings"),
            "limitations": lst(rec, "audit_limitations"),
            "visual_inspection_status": s(rec, "visual_inspection_status"),
            "command_results": lst(rec, "audit_command_results"),
            "evidence": lst(rec, "acceptance_mapping"),
            "review_blocker": s(rec, "fail_reason"),
        }),
        "delivery" => json!({
            "brief": s(rec, "product_summary"),
            "research": s(rec, "research_summary"),
            "design_system": s(rec, "design_summary"),
            "design_refs": lst(rec, "design_artifact_refs"),
            "stack_guide": s(rec, "implementation_guide_summary"),
            "implementation": s(rec, "implementation_summary"),
            "changed_files": lst(rec, "changed_files"),
            "audit": s(rec, "audit_summary"),
            "findings": lst(rec, "audit_findings"),
            "acceptance_results": lst(rec, "criterion_results"),
            "passed": lst(rec, "passed_criteria"),
            "failed": lst(rec, "failed_criteria"),
            "unverified": lst(rec, "unverified_criteria"),
            "not_applicable": lst(rec, "not_applicable_criteria"),
            "limitations": concat(&["audit_limitations", "stack_limitations", "research_limitations"]),
        }),
        _ => json!({}),
    };
    if let (Some(common), Value::Object(extra)) = (common.as_object_mut(), phase_data) {
        common.extend(extra);
    }
    format!(
        "[DYNAMIC WORKFLOW CONTEXT: {}]\n{}",
        phase.to_uppercase(),
        serde_json::to_string(&common).unwrap_or_default()
    )
}

// ── Acceptance validation ──────────────────────────────────────────────────

const ACCEPTANCE_STATUSES: &[&str] = &["PASS", "FAIL", "UNVERIFIED", "NOT_APPLICABLE"];

fn criterion_key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// agenthicc's `_validate_criterion_results`.
pub fn validate_criterion_results(
    criteria: &[String],
    results: &[String],
) -> Result<Vec<String>, String> {
    let mut expected: Vec<(String, String)> = Vec::new();
    for item in bounded_list(criteria) {
        let key = criterion_key(&item);
        if !expected.iter().any(|(k, _)| *k == key) {
            expected.push((key, item));
        }
    }
    if expected.is_empty() {
        return Err("The product brief must contain at least one acceptance criterion.".into());
    }
    let mut parsed: Vec<String> = Vec::new();
    for raw in bounded_list(results) {
        let parts: Vec<&str> = raw.splitn(3, '|').map(str::trim).collect();
        if parts.len() != 3 || !ACCEPTANCE_STATUSES.contains(&parts[0].to_uppercase().as_str()) {
            return Err(
                "Each result must be STATUS | exact criterion | evidence or reason.".into(),
            );
        }
        let (criterion, evidence) = (parts[1], parts[2]);
        let key = criterion_key(criterion);
        let Some((_, original)) = expected.iter().find(|(k, _)| *k == key) else {
            return Err(format!(
                "Result does not match a product-brief criterion: {}",
                criterion.chars().take(80).collect::<String>()
            ));
        };
        if parsed.contains(&key) {
            return Err(format!(
                "A criterion has more than one result: {}",
                original.chars().take(80).collect::<String>()
            ));
        }
        if evidence.is_empty() {
            return Err(format!(
                "Every criterion result needs evidence or a reason: {}",
                original.chars().take(80).collect::<String>()
            ));
        }
        parsed.push(key);
    }
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(k, _)| !parsed.contains(k))
        .map(|(_, item)| item.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Missing explicit status for: {}",
            missing
                .iter()
                .take(5)
                .copied()
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(bounded_list(results))
}

// ── The effect ─────────────────────────────────────────────────────────────

/// The tools this effect understands.
const TOOLS: &[&str] = &[
    "complete_product_discovery",
    "submit_design_research",
    "submit_design_system",
    "approve_design_system",
    "request_design_system_revision",
    "block_design_system",
    "submit_implementation_guide",
    "submit_experience_architecture",
    "complete_design_implementation",
    "submit_visual_ux_audit",
    "complete_design_remediation",
    "complete_design_acceptance",
    "complete_design_delivery",
];

/// Apply the effect for `ctx.tool`.
pub fn apply(ctx: &HookContext<'_>) -> Result<EffectOutcome, String> {
    let mut rec = record(ctx.data, RECORD);
    let summary = text(ctx, "summary");
    if TOOLS.contains(&ctx.tool) && summary.is_empty() {
        return Err(
            "A non-empty phase summary is required. Finish the phase and submit a \
             concise evidence-based summary."
                .into(),
        );
    }
    ensure_skill(ctx, &mut rec);
    let profile = field_str(&rec, "route_profile");
    let (route, note): (Route, String) = match ctx.tool {
        "complete_product_discovery" => {
            let scope = arg_str(ctx.args, "scope").to_lowercase();
            if !["broad", "targeted", "accessibility_audit"].contains(&scope.as_str()) {
                return Err("scope must be broad, targeted, or accessibility_audit".into());
            }
            let criteria = list(ctx, "acceptance_criteria");
            if criteria.is_empty() {
                return Err("At least one concrete acceptance criterion is required.".into());
            }
            put(&mut rec, "route_profile", scope.clone());
            put(&mut rec, "project_name", text_n(ctx, "project_name", 256));
            put(&mut rec, "project_type", text_n(ctx, "project_type", 256));
            put(&mut rec, "platform", text_n(ctx, "platform", 128));
            put(&mut rec, "stack", text_n(ctx, "stack", 512));
            put(&mut rec, "request_kind", text_n(ctx, "request_kind", 128));
            put(&mut rec, "product_summary", summary.clone());
            for key in [
                "target_users",
                "user_journeys",
                "required_surfaces",
                "functional_requirements",
                "ux_requirements",
                "accessibility_expectations",
                "technical_constraints",
                "existing_visual_conventions",
                "assumptions",
            ] {
                put(&mut rec, key, json!(list(ctx, key)));
            }
            put(&mut rec, "acceptance_criteria", json!(criteria));
            add_artifact(
                &mut rec,
                "product_discovery",
                "product_brief",
                &summary,
                &[],
            );
            let next = if scope == "broad" {
                "design_intelligence"
            } else {
                "targeted_design_research"
            };
            (
                Route::Phase(next.into()),
                format!("Recorded the {scope} product brief."),
            )
        }
        "submit_design_research" => {
            let queries = list(ctx, "queries");
            if queries.is_empty() {
                return Err(
                    "Record at least one query or explicit unavailable-capability check.".into(),
                );
            }
            put(&mut rec, "research_summary", summary.clone());
            put(&mut rec, "research_queries", json!(queries));
            for (field, arg) in [
                ("research_datasets", "datasets_consulted"),
                ("research_findings", "findings"),
                ("research_provenance", "source_provenance"),
                ("applicable_recommendations", "applicable_recommendations"),
                ("rejected_recommendations", "rejected_recommendations"),
                ("research_conflicts", "conflicts"),
                ("research_limitations", "limitations"),
                ("research_artifact_refs", "artifact_refs"),
            ] {
                put(&mut rec, field, json!(list(ctx, arg)));
            }
            put(
                &mut rec,
                "research_confidence",
                text_n(ctx, "confidence", 128),
            );
            add_artifact(
                &mut rec,
                ctx.phase,
                "design_research_report",
                &summary,
                &list(ctx, "artifact_refs"),
            );
            let next = if profile == "broad" {
                "design_system_synthesis"
            } else {
                "stack_adaptation"
            };
            (
                Route::Phase(next.into()),
                "Recorded the design research.".into(),
            )
        }
        "submit_design_system" => {
            let required = [
                ("visual direction", "visual_direction", false),
                ("style", "style", false),
                ("color tokens", "color_tokens", true),
                ("typography tokens", "typography_tokens", true),
                ("spacing rules", "spacing_rules", true),
                ("layout rules", "layout_rules", true),
                ("component rules", "component_rules", true),
                ("interaction states", "interaction_states", true),
                ("responsive behavior", "responsive_behavior", true),
                ("accessibility rules", "accessibility_rules", true),
                ("anti-patterns", "anti_patterns", true),
                ("artifact reference", "artifact_refs", true),
            ];
            let missing: Vec<&str> = required
                .iter()
                .filter(|(_, key, is_list)| {
                    if *is_list {
                        list(ctx, key).is_empty()
                    } else {
                        arg_str(ctx.args, key).is_empty()
                    }
                })
                .map(|(label, _, _)| *label)
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "The design-system submission is incomplete (missing: {}). Synthesize the \
                     missing system decisions and persist the master artifact before transition.",
                    missing.join(", ")
                ));
            }
            put(&mut rec, "design_summary", summary.clone());
            put(
                &mut rec,
                "design_artifact_refs",
                json!(list(ctx, "artifact_refs")),
            );
            put(
                &mut rec,
                "visual_direction",
                text_n(ctx, "visual_direction", 1_000),
            );
            put(&mut rec, "design_rationale", json!(list(ctx, "rationale")));
            put(&mut rec, "design_style", text_n(ctx, "style", 256));
            for key in [
                "color_tokens",
                "typography_tokens",
                "spacing_rules",
                "layout_rules",
                "component_rules",
                "interaction_states",
                "responsive_behavior",
                "accessibility_rules",
                "motion_rules",
                "anti_patterns",
            ] {
                put(&mut rec, key, json!(list(ctx, key)));
            }
            add_artifact(
                &mut rec,
                "design_system_synthesis",
                "master_design_system",
                &summary,
                &list(ctx, "artifact_refs"),
            );
            (
                Route::Phase("design_system_review".into()),
                "Recorded the master design system.".into(),
            )
        }
        "approve_design_system" | "request_design_system_revision" | "block_design_system" => {
            let attempts = field_u64(&rec, "design_review_attempts");
            let mut decision = ctx.tool.to_string();
            let findings = match ctx.tool {
                "request_design_system_revision" => {
                    let f = list(ctx, "actionable_findings");
                    if f.is_empty() {
                        return Err("A revision request requires actionable findings.".into());
                    }
                    if attempts >= max_review_attempts(ctx) {
                        // The hard retry cap turns a revision into a block.
                        decision = "block_design_system".into();
                    }
                    f
                }
                "block_design_system" => list(ctx, "blockers"),
                _ => list(ctx, "findings"),
            };
            put(&mut rec, "design_review_findings", json!(findings));
            put(&mut rec, "design_review_attempts", attempts + 1);
            add_artifact(&mut rec, "design_system_review", &decision, &summary, &[]);
            match decision.as_str() {
                "approve_design_system" => {
                    put(&mut rec, "design_review_status", "approved");
                    (
                        Route::Phase("stack_adaptation".into()),
                        "Design system approved.".into(),
                    )
                }
                "request_design_system_revision" => {
                    put(&mut rec, "design_review_status", "revise");
                    (
                        Route::Phase("design_system_synthesis".into()),
                        format!(
                            "Revision requested (attempt {} of {}). Synthesis runs again.",
                            attempts + 1,
                            max_review_attempts(ctx)
                        ),
                    )
                }
                _ => {
                    put(&mut rec, "design_review_status", "blocked");
                    let criteria = field_list(&rec, "acceptance_criteria");
                    put(&mut rec, "unverified_criteria", json!(criteria));
                    put(
                        &mut rec,
                        "fail_reason",
                        bounded_text(
                            &format!("Design-system quality review is blocked: {summary}"),
                            MAX_TEXT_CHARS,
                        ),
                    );
                    (
                        Route::Phase("design_acceptance".into()),
                        "Design-system review is blocked; acceptance records every criterion's \
                         status next."
                            .into(),
                    )
                }
            }
        }
        "submit_implementation_guide" => {
            let detected = text_n(ctx, "detected_stack", 512);
            if detected.is_empty() {
                return Err("Report the detected stack or explicitly state it is unknown.".into());
            }
            put(&mut rec, "implementation_guide_summary", summary.clone());
            put(&mut rec, "detected_stack", detected);
            for (field, arg) in [
                ("stack_evidence", "stack_evidence"),
                ("token_mappings", "token_mappings"),
                ("component_strategy", "component_strategy"),
                ("responsive_strategy", "responsive_strategy"),
                ("state_handling_conventions", "state_handling_conventions"),
                ("accessibility_strategy", "accessibility_strategy"),
                ("test_tools", "test_tools"),
                ("performance_constraints", "performance_constraints"),
                ("implementation_anti_patterns", "anti_patterns"),
                ("validation_commands", "validation_commands"),
                ("stack_limitations", "limitations"),
            ] {
                put(&mut rec, field, json!(list(ctx, arg)));
            }
            add_artifact(
                &mut rec,
                "stack_adaptation",
                "implementation_guide",
                &summary,
                &list(ctx, "artifact_refs"),
            );
            let next = if profile == "accessibility_audit" {
                "visual_ux_audit"
            } else {
                "experience_architecture"
            };
            (
                Route::Phase(next.into()),
                "Recorded the implementation guide.".into(),
            )
        }
        "submit_experience_architecture" => {
            for (field, arg) in [
                ("screen_inventory", "screen_inventory"),
                ("interaction_state_matrix", "interaction_state_matrix"),
                ("component_dependency_graph", "component_dependency_graph"),
                ("page_component_specs", "page_component_specs"),
                ("acceptance_mapping", "acceptance_mapping"),
                ("implementation_plan", "implementation_tasks"),
            ] {
                put(&mut rec, field, json!(list(ctx, arg)));
            }
            let feedback = field_str(&rec, "approval_feedback");
            put(&mut rec, "implementation_approval_feedback", feedback);
            add_artifact(
                &mut rec,
                "experience_architecture",
                "experience_specification",
                &summary,
                &list(ctx, "artifact_refs"),
            );
            (
                Route::Phase("design_implementation".into()),
                "Recorded the experience architecture.".into(),
            )
        }
        "complete_design_implementation" => {
            let deviations = list(ctx, "design_deviations");
            let reasons = list(ctx, "deviation_reasons");
            let joined: Vec<String> = deviations
                .iter()
                .enumerate()
                .map(|(i, d)| match reasons.get(i) {
                    Some(r) => format!("{d} — {r}"),
                    None => d.clone(),
                })
                .collect();
            put(&mut rec, "implementation_summary", summary.clone());
            put(
                &mut rec,
                "changed_files",
                json!(list(ctx, "modified_files")),
            );
            put(
                &mut rec,
                "implemented_states",
                json!(list(ctx, "implemented_states")),
            );
            put(&mut rec, "design_deviations", json!(joined));
            put(
                &mut rec,
                "remaining_implementation_work",
                json!(list(ctx, "remaining_work")),
            );
            let files = field_list(&rec, "changed_files");
            add_artifact(
                &mut rec,
                "design_implementation",
                "implementation_report",
                &summary,
                &files,
            );
            (
                Route::Phase("visual_ux_audit".into()),
                "Recorded the implementation.".into(),
            )
        }
        "submit_visual_ux_audit" => {
            let status = arg_str(ctx.args, "visual_inspection_status").to_lowercase();
            if !["inspected", "unavailable", "not_applicable", "not_required"]
                .contains(&status.as_str())
            {
                return Err("visual_inspection_status must be inspected, unavailable, \
                     not_applicable, or not_required"
                    .into());
            }
            let findings = list(ctx, "findings");
            put(&mut rec, "audit_summary", summary.clone());
            put(&mut rec, "audit_findings", json!(findings));
            put(&mut rec, "visual_inspection_status", status);
            put(
                &mut rec,
                "visual_evidence_refs",
                json!(list(ctx, "visual_evidence_refs")),
            );
            put(
                &mut rec,
                "audit_command_results",
                json!(list(ctx, "command_results")),
            );
            put(
                &mut rec,
                "audit_limitations",
                json!(list(ctx, "limitations")),
            );
            let mut mapping = field_list(&rec, "acceptance_mapping");
            mapping.extend(list(ctx, "acceptance_evidence"));
            put(
                &mut rec,
                "acceptance_mapping",
                json!(bounded_list(&mapping)),
            );
            add_artifact(
                &mut rec,
                "visual_ux_audit",
                "audit_report",
                &summary,
                &list(ctx, "visual_evidence_refs"),
            );
            let cycles = field_u64(&rec, "remediation_cycles");
            if profile != "accessibility_audit"
                && !findings.is_empty()
                && cycles < max_remediation_cycles(ctx)
            {
                put(&mut rec, "remediation_cycles", cycles + 1);
                (
                    Route::Phase("design_remediation".into()),
                    format!(
                        "Recorded {} finding(s); remediation cycle {} of {} starts next.",
                        findings.len(),
                        cycles + 1,
                        max_remediation_cycles(ctx)
                    ),
                )
            } else {
                (
                    Route::Phase("design_acceptance".into()),
                    "Recorded the audit; acceptance starts next.".into(),
                )
            }
        }
        "complete_design_remediation" => {
            put(&mut rec, "remediation_summary", summary.clone());
            put(
                &mut rec,
                "resolved_findings",
                json!(list(ctx, "resolved_findings")),
            );
            put(
                &mut rec,
                "unresolved_findings",
                json!(list(ctx, "unresolved_findings")),
            );
            let mut files = field_list(&rec, "changed_files");
            files.extend(list(ctx, "modified_files"));
            put(&mut rec, "changed_files", json!(unique(files)));
            put(
                &mut rec,
                "remediation_verification",
                json!(list(ctx, "verification_results")),
            );
            let files = field_list(&rec, "changed_files");
            add_artifact(
                &mut rec,
                "design_remediation",
                "remediation_report",
                &summary,
                &files,
            );
            (
                Route::Phase("visual_ux_audit".into()),
                "Recorded the remediation; the audit runs again.".into(),
            )
        }
        "complete_design_acceptance" => {
            let results = validate_criterion_results(
                &field_list(&rec, "acceptance_criteria"),
                &list(ctx, "criterion_results"),
            )?;
            let mut buckets: [Vec<String>; 4] = Default::default();
            for result in &results {
                let parts: Vec<&str> = result.splitn(3, '|').collect();
                if parts.len() != 3 {
                    continue;
                }
                let criterion = parts[1].trim().to_string();
                match parts[0].trim().to_uppercase().as_str() {
                    "PASS" => buckets[0].push(criterion),
                    "FAIL" => buckets[1].push(criterion),
                    "UNVERIFIED" => buckets[2].push(criterion),
                    "NOT_APPLICABLE" => buckets[3].push(criterion),
                    _ => {}
                }
            }
            put(&mut rec, "acceptance_summary", summary.clone());
            put(&mut rec, "criterion_results", json!(results));
            for (key, bucket) in [
                "passed_criteria",
                "failed_criteria",
                "unverified_criteria",
                "not_applicable_criteria",
            ]
            .into_iter()
            .zip(buckets)
            {
                put(&mut rec, key, json!(bucket));
            }
            let mut limitations = field_list(&rec, "audit_limitations");
            limitations.extend(list(ctx, "limitations"));
            put(
                &mut rec,
                "audit_limitations",
                json!(bounded_list(&limitations)),
            );
            add_artifact(
                &mut rec,
                "design_acceptance",
                "acceptance_report",
                &summary,
                &[],
            );
            (
                Route::Phase("delivery".into()),
                "Recorded every criterion's status; delivery starts next.".into(),
            )
        }
        "complete_design_delivery" => {
            put(&mut rec, "delivery_summary", summary.clone());
            add_artifact(&mut rec, "delivery", "final_report", &summary, &[]);
            let accepted = !field_list(&rec, "acceptance_criteria").is_empty()
                && field_list(&rec, "failed_criteria").is_empty()
                && field_list(&rec, "unverified_criteria").is_empty();
            if accepted {
                (
                    Route::End,
                    "Delivery complete: every criterion is accepted.".into(),
                )
            } else {
                if field_str(&rec, "fail_reason").is_empty() {
                    put(
                        &mut rec,
                        "fail_reason",
                        "Delivery is partial: at least one required acceptance criterion failed \
                         or is unverified.",
                    );
                }
                (
                    Route::Exit,
                    format!(
                        "The workflow ends BLOCKED: {} State the partial result plainly for the \
                         user and stop.",
                        field_str(&rec, "fail_reason")
                    ),
                )
            }
        }
        other => {
            return Err(format!(
                "the ui_ux_pro_max effect handles {}; '{other}' is not one of them",
                TOOLS.join(", ")
            ));
        }
    };
    if let Route::Phase(next) = &route {
        let context = render_phase_context(ctx, &rec, next);
        set_state(ctx.data, &[("phase_context", context)]);
    }
    save_record(ctx.data, RECORD, rec);
    Ok(EffectOutcome {
        note,
        route: Some(route),
    })
}

// ── Implementation-plan approval ───────────────────────────────────────────

/// The `implementation_plan_approved` guard.
pub fn check_implementation_approved(ctx: &HookContext<'_>) -> Result<(), String> {
    let approved = ctx.control.lock().map(|c| c.plan_approved).unwrap_or(false);
    if approved {
        Ok(())
    } else {
        Err(
            "Request and receive implementation-plan approval before submitting this phase \
             (call request_implementation_approval(plan))."
                .into(),
        )
    }
}

/// The `ui_ux_pro_max` tool group.
pub fn ui_ux_pro_max_tools(
    env: &TransitionEnv,
    question_gate: Option<Arc<dyn UserQuestionGate>>,
) -> Vec<Arc<ErasedTool>> {
    vec![Arc::new(ImplementationApprovalTool {
        env: env.clone(),
        question_gate,
    }) as Arc<ErasedTool>]
}

/// agenthicc's `request_implementation_approval` — not a transition.
struct ImplementationApprovalTool {
    env: TransitionEnv,
    question_gate: Option<Arc<dyn UserQuestionGate>>,
}

#[async_trait::async_trait]
impl Tool for ImplementationApprovalTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "request_implementation_approval"
    }

    fn description(&self) -> &str {
        "Request approval for the concrete screen/component implementation plan. Does not change \
         phase; submit_experience_architecture(...) is only accepted after approval."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "plan": { "type": "string", "description": "The implementation plan." } },
            "required": ["plan"],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let plan = bounded_text(
            input
                .get("plan")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            MAX_TEXT_CHARS,
        );
        if plan.is_empty() {
            return Ok(ToolResult::error(
                json!({"ok": false, "error": "The implementation plan must not be empty."})
                    .to_string(),
                &ctx.tool_use_id,
            ));
        }
        let (granted, feedback) = if self.env.approval_gate.is_none() {
            (
                true,
                "No interactive approval service is configured for this run.".to_string(),
            )
        } else {
            let granted = ask_user_approval(
                self.env.approval_gate.as_ref(),
                self.env.signals.as_ref(),
                "UI/UX Pro Max Implementation Plan",
                &ctx.tool_use_id,
                &json!({ "plan": plan }),
            )
            .await;
            let mut feedback = String::new();
            if !granted {
                if let Some(q) = &self.question_gate {
                    let outcome = q
                        .ask(&[UserQuestion::free_text(
                            "feedback",
                            "What should change in the plan?",
                        )])
                        .await;
                    if let Some(answers) = outcome.answers() {
                        feedback = bounded_text(
                            answers.first().map_or("", String::as_str),
                            MAX_TEXT_CHARS,
                        );
                    }
                }
            }
            (granted, feedback)
        };
        if let Ok(mut c) = self.env.control.lock() {
            c.plan_approved = granted;
            c.approved_plan = granted.then(|| plan.clone());
        }
        let mut rec = record(&self.env.data, RECORD);
        put(&mut rec, "approval_feedback", feedback.clone());
        save_record(&self.env.data, RECORD, rec);
        let body = json!({ "ok": granted, "approved": granted, "feedback": feedback }).to_string();
        Ok(if granted {
            ToolResult::ok(body, &ctx.tool_use_id)
        } else {
            ToolResult::error(body, &ctx.tool_use_id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::groups::{RunData, RunMeta};
    use crate::phased::tools::{PhaseControl, SharedControl};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn data_with(params: Value) -> RunData {
        let tmp = std::env::temp_dir();
        RunData::new(BTreeMap::new(), &tmp).with_meta(RunMeta {
            run_id: "r".into(),
            workflow: "ui_ux_pro_max".into(),
            intent: "Improve the checkout".into(),
            params: params.as_object().cloned().unwrap_or_default(),
            tool_names: Vec::new(),
        })
    }

    fn call(data: &RunData, phase: &str, tool: &str, args: Value) -> Result<EffectOutcome, String> {
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        apply(&HookContext {
            data,
            control: &control,
            args: &args,
            phase,
            tool,
        })
    }

    fn next(out: &EffectOutcome) -> String {
        match &out.route {
            Some(Route::Phase(p)) => p.clone(),
            Some(Route::End) => "END".into(),
            Some(Route::Exit) => "EXIT".into(),
            None => "-".into(),
        }
    }

    fn discovery(scope: &str) -> Value {
        json!({
            "summary": "A checkout",
            "scope": scope,
            "acceptance_criteria": ["Checkout works on mobile", "Errors are announced"],
        })
    }

    #[test]
    fn profiles_choose_the_research_and_implementation_routes() {
        for (scope, research, after_stack) in [
            ("broad", "design_intelligence", "experience_architecture"),
            (
                "targeted",
                "targeted_design_research",
                "experience_architecture",
            ),
            (
                "accessibility_audit",
                "targeted_design_research",
                "visual_ux_audit",
            ),
        ] {
            let data = data_with(json!({}));
            let out = call(
                &data,
                "product_discovery",
                "complete_product_discovery",
                discovery(scope),
            )
            .unwrap();
            assert_eq!(next(&out), research);
            let out = call(
                &data,
                research,
                "submit_design_research",
                json!({"summary": "s", "queries": ["q"]}),
            )
            .unwrap();
            let expected = if scope == "broad" {
                "design_system_synthesis"
            } else {
                "stack_adaptation"
            };
            assert_eq!(next(&out), expected);
            let out = call(
                &data,
                "stack_adaptation",
                "submit_implementation_guide",
                json!({"summary": "s", "detected_stack": "rust"}),
            )
            .unwrap();
            assert_eq!(next(&out), after_stack, "{scope}");
            for phase in phases_for(scope) {
                assert!(phases_for(scope).contains(phase));
            }
        }
    }

    #[test]
    fn discovery_validates_scope_and_criteria() {
        let data = data_with(json!({}));
        let err = call(
            &data,
            "product_discovery",
            "complete_product_discovery",
            discovery("huge"),
        )
        .unwrap_err();
        assert!(err.contains("scope must be"), "{err}");
        let mut bad = discovery("broad");
        bad["acceptance_criteria"] = json!([]);
        assert!(
            call(
                &data,
                "product_discovery",
                "complete_product_discovery",
                bad
            )
            .is_err()
        );
        let mut blank = discovery("broad");
        blank["summary"] = json!("  ");
        assert!(
            call(
                &data,
                "product_discovery",
                "complete_product_discovery",
                blank
            )
            .unwrap_err()
            .contains("summary")
        );
    }

    #[test]
    fn revisions_stop_at_the_review_budget_and_block() {
        let data = data_with(json!({"max_design_review_attempts": 1}));
        call(
            &data,
            "product_discovery",
            "complete_product_discovery",
            discovery("broad"),
        )
        .unwrap();
        let revise = json!({"summary": "s", "actionable_findings": ["contrast"]});
        let out = call(
            &data,
            "design_system_review",
            "request_design_system_revision",
            revise.clone(),
        )
        .unwrap();
        assert_eq!(next(&out), "design_system_synthesis");
        let out = call(
            &data,
            "design_system_review",
            "request_design_system_revision",
            revise,
        )
        .unwrap();
        assert_eq!(
            next(&out),
            "design_acceptance",
            "the cap turns the revision into a block"
        );
        let rec = record(&data, RECORD);
        assert_eq!(rec["design_review_status"], "blocked");
        assert_eq!(
            rec["unverified_criteria"],
            json!(["Checkout works on mobile", "Errors are announced"])
        );
        assert!(
            field_str(&rec, "fail_reason").starts_with("Design-system quality review is blocked")
        );
    }

    #[test]
    fn audit_findings_enter_remediation_while_cycles_remain() {
        let data = data_with(json!({"max_remediation_cycles": 1}));
        call(
            &data,
            "product_discovery",
            "complete_product_discovery",
            discovery("targeted"),
        )
        .unwrap();
        let audit = json!({"summary": "s", "findings": ["High | nav | e | x | y | fix | check"], "visual_inspection_status": "Inspected"});
        let out = call(
            &data,
            "visual_ux_audit",
            "submit_visual_ux_audit",
            audit.clone(),
        )
        .unwrap();
        assert_eq!(next(&out), "design_remediation");
        let state_ctx = data.get("state").unwrap()["phase_context"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            state_ctx.starts_with("[DYNAMIC WORKFLOW CONTEXT: DESIGN_REMEDIATION]\n{"),
            "{state_ctx}"
        );
        assert!(state_ctx.contains("\"max_cycles\":1"));
        let out = call(
            &data,
            "design_remediation",
            "complete_design_remediation",
            json!({"summary": "fixed"}),
        )
        .unwrap();
        assert_eq!(next(&out), "visual_ux_audit");
        let out = call(&data, "visual_ux_audit", "submit_visual_ux_audit", audit).unwrap();
        assert_eq!(
            next(&out),
            "design_acceptance",
            "the remediation budget is spent"
        );

        let bad = json!({"summary": "s", "visual_inspection_status": "looked"});
        assert!(call(&data, "visual_ux_audit", "submit_visual_ux_audit", bad).is_err());
    }

    #[test]
    fn an_accessibility_audit_never_remediates() {
        let data = data_with(json!({}));
        call(
            &data,
            "product_discovery",
            "complete_product_discovery",
            discovery("accessibility_audit"),
        )
        .unwrap();
        let audit = json!({"summary": "s", "findings": ["Critical | form | e | x | y | fix | check"], "visual_inspection_status": "unavailable"});
        let out = call(&data, "visual_ux_audit", "submit_visual_ux_audit", audit).unwrap();
        assert_eq!(next(&out), "design_acceptance");
    }

    #[test]
    fn criterion_results_must_cover_every_criterion_exactly_once() {
        let criteria = vec![
            "Checkout works on mobile".to_string(),
            "Errors are announced".to_string(),
        ];
        let ok = validate_criterion_results(
            &criteria,
            &[
                "PASS | checkout works  on MOBILE | screenshot".into(),
                "unverified | Errors are announced | no screen reader".into(),
            ],
        );
        assert!(ok.is_ok(), "{ok:?}");
        for (results, message) in [
            (
                vec!["DONE | Checkout works on mobile | x".to_string()],
                "STATUS | exact criterion",
            ),
            (
                vec!["PASS | Something else | x".to_string()],
                "does not match",
            ),
            (
                vec![
                    "PASS | Errors are announced | x".to_string(),
                    "FAIL | Errors are announced | y".to_string(),
                ],
                "more than one",
            ),
            (
                vec!["PASS | Errors are announced | ".to_string()],
                "needs evidence",
            ),
            (
                vec!["PASS | Errors are announced | x".to_string()],
                "Missing explicit status for: Checkout works on mobile",
            ),
        ] {
            let err = validate_criterion_results(&criteria, &results).unwrap_err();
            assert!(err.contains(message), "{err}");
        }
        assert!(
            validate_criterion_results(&[], &[])
                .unwrap_err()
                .contains("at least one")
        );
    }

    #[test]
    fn delivery_completes_only_when_every_criterion_is_accepted() {
        let data = data_with(json!({}));
        call(
            &data,
            "product_discovery",
            "complete_product_discovery",
            discovery("targeted"),
        )
        .unwrap();
        let results = json!({"summary": "s", "criterion_results": [
            "PASS | Checkout works on mobile | e2e run",
            "NOT_APPLICABLE | Errors are announced | no errors in scope"
        ]});
        call(
            &data,
            "design_acceptance",
            "complete_design_acceptance",
            results,
        )
        .unwrap();
        let out = call(
            &data,
            "delivery",
            "complete_design_delivery",
            json!({"summary": "done"}),
        )
        .unwrap();
        assert_eq!(next(&out), "END");

        let data = data_with(json!({}));
        call(
            &data,
            "product_discovery",
            "complete_product_discovery",
            discovery("targeted"),
        )
        .unwrap();
        let results = json!({"summary": "s", "criterion_results": [
            "PASS | Checkout works on mobile | e2e run",
            "UNVERIFIED | Errors are announced | no screen reader available"
        ]});
        call(
            &data,
            "design_acceptance",
            "complete_design_acceptance",
            results,
        )
        .unwrap();
        let out = call(
            &data,
            "delivery",
            "complete_design_delivery",
            json!({"summary": "partial"}),
        )
        .unwrap();
        assert_eq!(next(&out), "EXIT");
        assert!(
            out.note.contains("BLOCKED: Delivery is partial"),
            "{}",
            out.note
        );
    }

    #[test]
    fn deviations_carry_their_reasons() {
        let data = data_with(json!({}));
        call(&data, "design_implementation", "complete_design_implementation", json!({
            "summary": "s", "design_deviations": ["kept old font", "no motion"], "deviation_reasons": ["licensing"]
        })).unwrap();
        assert_eq!(
            record(&data, RECORD)["design_deviations"],
            json!(["kept old font — licensing", "no motion"])
        );
    }

    #[tokio::test]
    async fn the_approval_tool_opens_the_guard_for_the_phase_run() {
        let data = data_with(json!({}));
        let env = TransitionEnv {
            control: Arc::new(Mutex::new(PhaseControl::default())),
            data: data.clone(),
            approval_gate: None,
            signals: None,
            phase: "experience_architecture".into(),
        };
        let args = json!({});
        let hook = |env: &TransitionEnv| {
            check_implementation_approved(&HookContext {
                data: &env.data,
                control: &env.control,
                args: &args,
                phase: "experience_architecture",
                tool: "submit_experience_architecture",
            })
        };
        assert!(hook(&env).is_err());
        let tools = ui_ux_pro_max_tools(&env, None);
        let r = tools[0]
            .call(
                json!({"plan": "1. checkout screen"}),
                &ToolContext::new("t"),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("No interactive approval service"));
        assert!(hook(&env).is_ok());
    }

    #[test]
    fn bounded_text_and_lists_match_the_runner_limits() {
        assert_eq!(bounded_text(" a\r\nb\u{7}\tc ", 100), "a\nb\tc");
        assert_eq!(bounded_text("abcdef", 3), "abc");
        let many: Vec<String> = (0..50).map(|i| format!("{i}")).collect();
        assert_eq!(bounded_list(&many).len(), MAX_LIST_ITEMS);
    }
}
