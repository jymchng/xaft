//! The `site_imitate` effect and the `mobile_verified` guard — the
//! bookkeeping of agenthicc's `SiteImitateRunner`
//! (`workflows/site_imitate/runner.py`).
//!
//! The runner's PLAN phase decides how many parts the site needs; it then
//! drives one BUILD and one VERIFY phase per component and a final site-wide
//! verification. The cursor lives in the run record `site_imitate`
//! (`component_plan`, `component_index`) and the current component is
//! mirrored into the prompt state, so the build/verify prompts name exactly
//! the component being worked on:
//!
//! | state key | value |
//! |---|---|
//! | `component_name`, `component_spec`, `component_spec_or_plan`, `component_verify` | the current component, with the runner's fallbacks |
//! | `component_number`, `component_count`, `component_label` | `i`, `n`, `"i/n - name"` |
//! | `component_plan` | the whole plan, `_component_plan_text` |
//! | `component_inventory` | the analysis' Named Component Inventory, or `(none recorded)` |
//! | `project_path` | the scaffolded project |
//!
//! | tool | effect |
//! |---|---|
//! | `submit_analysis` | records the analysis and component inventory |
//! | `submit_plan` | parses `NAME \| BUILD SPEC \| VERIFY CHECK` lines into the plan, cursor 0 |
//! | `request_reanalysis` | records the question (the TOML routes back to `analyze`) |
//! | `scaffold_complete` | records the project; routes to `build`, or `final_verify` when no component is planned |
//! | `component_verified` | advances the cursor; routes to `build` while components remain, else `final_verify` |
//! | `component_verification_failed` | records the errors (the TOML routes back to `build`) |
//! | `final_verify_passed` | records the build summary |
//! | `final_verify_failed` | records the errors and moves the cursor to the last component |

use serde_json::{Map, Value, json};

use super::hooks::{
    EffectOutcome, HookContext, arg_list, arg_str, field_u64, record, save_record, set_state,
};

const RECORD: &str = "site_imitate";

/// The tools this effect understands.
const TOOLS: &[&str] = &[
    "submit_analysis",
    "submit_plan",
    "request_reanalysis",
    "scaffold_complete",
    "component_built",
    "component_verified",
    "component_verification_failed",
    "final_verify_passed",
    "final_verify_failed",
];

/// agenthicc's `_mobile_verification_error`: a success summary must evidence
/// the responsive check.
pub fn check_mobile_verified(ctx: &HookContext<'_>) -> Result<(), String> {
    let summary = arg_str(ctx.args, "summary").to_lowercase();
    if ["mobile", "responsive", "viewport"]
        .iter()
        .any(|m| summary.contains(m))
    {
        Ok(())
    } else {
        Err(
            "Mobile responsiveness was not evidenced. Re-run the verification at narrow, \
             tablet, and desktop viewports and mention the responsive/overflow results in the \
             summary."
                .into(),
        )
    }
}

/// One planned component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Canonical name.
    pub name: String,
    /// What to build.
    pub build: String,
    /// How to verify it.
    pub verify: String,
}

/// agenthicc's `_parse_component_plan`: one `NAME | BUILD SPEC | VERIFY
/// CHECK` line per component; missing sections fall back to the name alone.
pub fn parse_component_plan(lines: &[String]) -> Vec<Component> {
    let mut plan: Vec<Component> = Vec::new();
    for item in lines {
        let line = item.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '|').map(str::trim).collect();
        let name = match parts[0] {
            "" => format!("component-{}", plan.len() + 1),
            n => n.to_string(),
        };
        plan.push(Component {
            name,
            build: parts.get(1).copied().unwrap_or_default().to_string(),
            verify: parts.get(2).copied().unwrap_or_default().to_string(),
        });
    }
    plan
}

/// agenthicc's `_component_plan_text`.
pub fn component_plan_text(plan: &[Component]) -> String {
    if plan.is_empty() {
        return "(no components planned)".to_string();
    }
    plan.iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "{}. {}\n   build : {}\n   verify: {}",
                i + 1,
                if c.name.is_empty() {
                    format!("component-{}", i + 1)
                } else {
                    c.name.clone()
                },
                or(&c.build, "(from plan)"),
                or(&c.verify, "(compiles and matches the reference)")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn load_plan(rec: &Map<String, Value>) -> Vec<Component> {
    match rec.get("component_plan") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|c| Component {
                name: c["name"].as_str().unwrap_or_default().to_string(),
                build: c["build"].as_str().unwrap_or_default().to_string(),
                verify: c["verify"].as_str().unwrap_or_default().to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn store_plan(rec: &mut Map<String, Value>, plan: &[Component]) {
    rec.insert(
        "component_plan".into(),
        Value::Array(
            plan.iter()
                .map(|c| json!({"name": c.name, "build": c.build, "verify": c.verify}))
                .collect(),
        ),
    );
}

/// Mirror the cursor's component into the prompt state — what the runner's
/// `_build_component` / `_verify_component` format into their prompts.
fn publish_cursor(ctx: &HookContext<'_>, plan: &[Component], index: usize) {
    let fallback = Component {
        name: "component".into(),
        build: String::new(),
        verify: String::new(),
    };
    let comp = plan.get(index).unwrap_or(&fallback);
    set_state(
        ctx.data,
        &[
            ("component_name", or(&comp.name, "?").to_string()),
            (
                "component_spec",
                or(&comp.build, "(from the plan)").to_string(),
            ),
            (
                "component_spec_or_plan",
                or(&comp.build, "(follow the plan)").to_string(),
            ),
            (
                "component_verify",
                or(&comp.verify, "(compiles and matches the reference)").to_string(),
            ),
            ("component_number", (index + 1).to_string()),
            ("component_count", plan.len().to_string()),
            (
                "component_label",
                format!(
                    "{}/{} - {}",
                    index + 1,
                    plan.len(),
                    or(&comp.name, "component")
                ),
            ),
            ("component_plan", component_plan_text(plan)),
        ],
    );
}

fn artifact(rec: &mut Map<String, Value>, key: String, value: String) {
    let mut artifacts = match rec.get("artifacts") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    artifacts.insert(key, json!(value));
    rec.insert("artifacts".into(), Value::Object(artifacts));
}

/// Apply the effect for `ctx.tool`.
pub fn apply(ctx: &HookContext<'_>) -> Result<EffectOutcome, String> {
    let mut rec = record(ctx.data, RECORD);
    let plan = load_plan(&rec);
    let index = field_u64(&rec, "component_index") as usize;
    let outcome = match ctx.tool {
        "submit_analysis" => {
            let analysis = arg_str(ctx.args, "analysis");
            if analysis.is_empty() {
                return Err("The analysis was rejected: it must not be empty. Call \
                     submit_analysis(analysis) with the site analysis."
                    .into());
            }
            let inventory = arg_str(ctx.args, "components");
            rec.insert("analysis".into(), json!(analysis));
            rec.insert("component_inventory".into(), json!(inventory));
            set_state(
                ctx.data,
                &[(
                    "component_inventory",
                    or(&inventory, "(none recorded)").to_string(),
                )],
            );
            EffectOutcome::note("Analysis recorded. The plan phase starts next.")
        }
        "submit_plan" => {
            let text = arg_str(ctx.args, "plan");
            let lines = arg_list(ctx.args, "components");
            if text.is_empty() {
                return Err("The plan was rejected: it must not be empty. Call \
                     submit_plan(plan, components=[...]) with the build plan."
                    .into());
            }
            if lines.is_empty() {
                return Err(
                    "The plan was rejected: it must include at least one component. \
                     Call submit_plan(plan, components=[...]) with the ordered component list."
                        .into(),
                );
            }
            let mut parsed = parse_component_plan(&lines);
            if parsed.is_empty() {
                // No structured list: the plan itself is one component.
                parsed = vec![Component {
                    name: "The Website".into(),
                    build: text.clone(),
                    verify: String::new(),
                }];
            }
            rec.insert("plan".into(), json!(text));
            store_plan(&mut rec, &parsed);
            rec.insert("component_index".into(), json!(0));
            publish_cursor(ctx, &parsed, 0);
            EffectOutcome::note(format!(
                "Plan recorded with {} components. The scaffold phase starts next.",
                parsed.len()
            ))
        }
        "request_reanalysis" => {
            artifact(
                &mut rec,
                "reanalysis_question".into(),
                arg_str(ctx.args, "question"),
            );
            EffectOutcome::note("Re-analysis requested. Returning to analyze phase.")
        }
        "scaffold_complete" => {
            let path = arg_str(ctx.args, "path");
            if path.is_empty() {
                return Err(
                    "The project path must not be empty. Call scaffold_complete(path) \
                     with the project directory path."
                        .into(),
                );
            }
            rec.insert("project_path".into(), json!(path));
            set_state(ctx.data, &[("project_path", path)]);
            if plan.is_empty() {
                EffectOutcome::to_phase(
                    "final_verify",
                    "Scaffold complete. No component is planned; the final verify phase starts next.",
                )
            } else {
                publish_cursor(ctx, &plan, index);
                EffectOutcome::to_phase("build", "Scaffold complete. The build phase starts next.")
            }
        }
        "component_built" => {
            EffectOutcome::note("Component built. The verify phase for this component starts next.")
        }
        "component_verified" => {
            artifact(
                &mut rec,
                format!("verify_{index}"),
                arg_str(ctx.args, "summary"),
            );
            let next = index + 1;
            rec.insert("component_index".into(), json!(next));
            if next < plan.len() {
                publish_cursor(ctx, &plan, next);
                EffectOutcome::to_phase(
                    "build",
                    "Component verified. Advancing to the next component.",
                )
            } else {
                EffectOutcome::to_phase(
                    "final_verify",
                    "Component verified. Every planned component is built; the final verify \
                     phase starts next.",
                )
            }
        }
        "component_verification_failed" => {
            artifact(
                &mut rec,
                format!("verify_errors_{index}"),
                arg_str(ctx.args, "errors"),
            );
            EffectOutcome::note("Errors recorded. Re-entering this component's build phase.")
        }
        "final_verify_passed" => {
            artifact(
                &mut rec,
                "verify_summary".into(),
                arg_str(ctx.args, "summary"),
            );
            EffectOutcome::note("Final verify passed. The site is complete.")
        }
        "final_verify_failed" => {
            artifact(
                &mut rec,
                "verify_errors".into(),
                arg_str(ctx.args, "errors"),
            );
            if !plan.is_empty() {
                let last = plan.len() - 1;
                rec.insert("component_index".into(), json!(last));
                publish_cursor(ctx, &plan, last);
            }
            EffectOutcome::note("Errors recorded. Re-entering the last component's build phase.")
        }
        other => {
            return Err(format!(
                "the site_imitate effect handles {}; '{other}' is not one of them",
                TOOLS.join(", ")
            ));
        }
    };
    save_record(ctx.data, RECORD, rec);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::groups::RunData;
    use crate::phased::hooks::Route;
    use crate::phased::tools::{PhaseControl, SharedControl};
    use std::sync::{Arc, Mutex};

    fn call(data: &RunData, tool: &str, args: Value) -> Result<EffectOutcome, String> {
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        apply(&HookContext {
            data,
            control: &control,
            args: &args,
            phase: "p",
            tool,
        })
    }

    fn state(data: &RunData, key: &str) -> String {
        data.get("state")
            .and_then(|s| s.get(key).and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }

    fn route(out: &EffectOutcome) -> Option<&str> {
        match &out.route {
            Some(Route::Phase(p)) => Some(p.as_str()),
            _ => None,
        }
    }

    #[test]
    fn component_lines_parse_like_the_runner() {
        let plan = parse_component_plan(&[
            "Header | sticky bar | header is sticky".into(),
            "Footer".into(),
            " | nameless | check".into(),
            "  ".into(),
            "Nav | Sheet side=\"left\" | toggles | and more".into(),
        ]);
        assert_eq!(plan.len(), 4);
        assert_eq!(plan[1].build, "");
        assert_eq!(plan[2].name, "component-3");
        assert_eq!(plan[3].verify, "toggles | and more", "split at most twice");
        let text = component_plan_text(&plan[1..2]);
        assert_eq!(
            text,
            "1. Footer\n   build : (from plan)\n   verify: (compiles and matches the reference)"
        );
    }

    #[test]
    fn the_cursor_walks_every_component_then_final_verify() {
        let data = RunData::default();
        call(
            &data,
            "submit_plan",
            json!({"plan": "build it", "components": ["Header | bar | sticky", "Footer | links | renders"]}),
        )
        .unwrap();
        assert_eq!(state(&data, "component_label"), "1/2 - Header");
        let out = call(&data, "scaffold_complete", json!({"path": "app"})).unwrap();
        assert_eq!(route(&out), Some("build"));
        assert_eq!(state(&data, "project_path"), "app");

        // A failed verification keeps the cursor.
        call(
            &data,
            "component_verification_failed",
            json!({"errors": "tsc"}),
        )
        .unwrap();
        assert_eq!(state(&data, "component_name"), "Header");

        let out = call(&data, "component_verified", json!({"summary": "mobile ok"})).unwrap();
        assert_eq!(route(&out), Some("build"));
        assert_eq!(state(&data, "component_label"), "2/2 - Footer");
        assert_eq!(state(&data, "component_verify"), "renders");

        let out = call(&data, "component_verified", json!({"summary": "mobile ok"})).unwrap();
        assert_eq!(route(&out), Some("final_verify"));

        // A failed final verify returns to the last component.
        call(
            &data,
            "final_verify_failed",
            json!({"errors": "build broke"}),
        )
        .unwrap();
        assert_eq!(state(&data, "component_label"), "2/2 - Footer");
        let rec = record(&data, RECORD);
        assert_eq!(rec["component_index"], 1);
        assert_eq!(rec["artifacts"]["verify_errors"], "build broke");
    }

    #[test]
    fn scaffolding_without_a_plan_goes_straight_to_final_verify() {
        let data = RunData::default();
        let out = call(&data, "scaffold_complete", json!({"path": "app"})).unwrap();
        assert_eq!(route(&out), Some("final_verify"));
    }

    #[test]
    fn empty_submissions_are_refused_with_the_runners_fix() {
        let data = RunData::default();
        let err = call(&data, "submit_plan", json!({"plan": "p", "components": []})).unwrap_err();
        assert!(err.contains("at least one component"), "{err}");
        assert!(call(&data, "submit_analysis", json!({"analysis": " "})).is_err());
        assert!(call(&data, "unrelated", json!({})).is_err());
    }

    #[test]
    fn success_summaries_must_evidence_responsive_checks() {
        let data = RunData::default();
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        let check = |summary: &str| {
            let args = json!({ "summary": summary });
            check_mobile_verified(&HookContext {
                data: &data,
                control: &control,
                args: &args,
                phase: "verify",
                tool: "component_verified",
            })
        };
        assert!(check("tsc passes").is_err());
        assert!(check("Checked at 375px Viewport, no overflow").is_ok());
        assert!(check("responsive nav works").is_ok());
    }
}
