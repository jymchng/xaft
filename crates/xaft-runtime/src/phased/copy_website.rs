//! The `copy_website` effect — the bookkeeping of agenthicc's
//! `CopyWebsiteRunner` (`workflows/copy_website/runner.py`).
//!
//! The runner keeps a typed `CopyContext` and fills later phases' task text
//! from it. Here the same values live in the run record `copy_website` and are
//! mirrored into the prompt state (`{state.<key>}`):
//!
//! | tool | effect |
//! |---|---|
//! | `submit_target` | `target_url`, `target_scope` (`sitemap` when blank), `target_pages` |
//! | `submit_study_report` | replaces `target_pages` when the study found pages; records screenshots |
//! | `submit_pages` | `pages_built` |
//! | `reject_parity` | counts the fix iteration and sets `fix_context`, the text the runner's `_fix_context` splices into the implementation prompts |
//! | `approve_parity` | records the parity summary and clears `fix_context` |
//!
//! None of these route: copy_website's graph is linear apart from the
//! `reject_parity` → `implement_layout` edge the TOML declares.

use serde_json::json;

use super::hooks::{
    EffectOutcome, HookContext, arg_str, field_list, field_u64, put_list, record, save_record,
    set_state,
};

const RECORD: &str = "copy_website";

/// The tools this effect understands.
const TOOLS: &[&str] = &[
    "submit_target",
    "submit_study_report",
    "submit_pages",
    "reject_parity",
    "approve_parity",
];

/// agenthicc's `[p.strip() for p in pages.split(",") if p.strip()]`.
fn split_pages(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn pages_line(pages: &[String]) -> String {
    if pages.is_empty() {
        "from sitemap".to_string()
    } else {
        pages.join(", ")
    }
}

/// Apply the effect for `ctx.tool`.
pub fn apply(ctx: &HookContext<'_>) -> Result<EffectOutcome, String> {
    let mut rec = record(ctx.data, RECORD);
    match ctx.tool {
        "submit_target" => {
            let url = arg_str(ctx.args, "url");
            if url.is_empty() {
                return Err("The URL was rejected: it must not be empty. Call \
                     submit_target(url, scope, pages) with a non-empty URL."
                    .into());
            }
            let scope = match arg_str(ctx.args, "scope") {
                s if s.is_empty() => "sitemap".to_string(),
                s => s,
            };
            let pages = split_pages(&arg_str(ctx.args, "pages"));
            rec.insert("target_url".into(), json!(url));
            rec.insert("scope".into(), json!(scope));
            put_list(&mut rec, "target_pages", &pages);
            set_state(
                ctx.data,
                &[
                    ("target_url", url),
                    ("target_scope", scope),
                    ("target_pages", pages_line(&pages)),
                ],
            );
            save_record(ctx.data, RECORD, rec);
            Ok(EffectOutcome::note(
                "Target recorded. The site_study phase starts next.",
            ))
        }
        "submit_study_report" => {
            let pages = split_pages(&arg_str(ctx.args, "pages"));
            if !pages.is_empty() {
                put_list(&mut rec, "target_pages", &pages);
            }
            let shots = split_pages(&arg_str(ctx.args, "screenshots"));
            put_list(&mut rec, "screenshots", &shots);
            let target_pages = field_list(&rec, "target_pages");
            set_state(ctx.data, &[("target_pages", pages_line(&target_pages))]);
            save_record(ctx.data, RECORD, rec);
            Ok(EffectOutcome::note(
                "Study report recorded. The design_spec phase starts next.",
            ))
        }
        "submit_pages" => {
            let built = split_pages(&arg_str(ctx.args, "pages_built"));
            put_list(&mut rec, "pages_built", &built);
            set_state(ctx.data, &[("pages_built", built.join(", "))]);
            save_record(ctx.data, RECORD, rec);
            Ok(EffectOutcome::note(
                "Pages recorded. The implement_data phase starts next.",
            ))
        }
        "reject_parity" => {
            let iteration = field_u64(&rec, "fix_iterations") + 1;
            let reason = match arg_str(ctx.args, "reason") {
                r if r.is_empty() => "Parity rejected".to_string(),
                r => r,
            };
            rec.insert("fix_iterations".into(), json!(iteration));
            rec.insert("fix_reason".into(), json!(reason));
            set_state(
                ctx.data,
                &[
                    ("fix_iterations", iteration.to_string()),
                    (
                        "fix_context",
                        format!(
                            "Parity verification was rejected (iteration {iteration}): {reason}\n\
                             Fix exactly these issues before calling the transition tool."
                        ),
                    ),
                ],
            );
            save_record(ctx.data, RECORD, rec);
            Ok(EffectOutcome::note(format!(
                "Parity rejected (iteration {iteration}). implement_layout runs again with the \
                 reason."
            )))
        }
        "approve_parity" => {
            rec.insert("parity_summary".into(), json!(arg_str(ctx.args, "summary")));
            set_state(ctx.data, &[("fix_context", String::new())]);
            save_record(ctx.data, RECORD, rec);
            Ok(EffectOutcome::note(
                "Parity approved. The final_report phase starts next.",
            ))
        }
        other => Err(format!(
            "the copy_website effect handles {}; '{other}' is not one of them",
            TOOLS.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::groups::RunData;
    use crate::phased::tools::{PhaseControl, SharedControl};
    use serde_json::{Value, json};
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

    #[test]
    fn the_target_defaults_its_scope_and_pages_like_the_runner() {
        let data = RunData::default();
        call(
            &data,
            "submit_target",
            json!({"url": " https://a.test ", "scope": "", "pages": ""}),
        )
        .unwrap();
        assert_eq!(state(&data, "target_url"), "https://a.test");
        assert_eq!(state(&data, "target_scope"), "sitemap");
        assert_eq!(state(&data, "target_pages"), "from sitemap");

        call(
            &data,
            "submit_study_report",
            json!({"report": "r", "pages": "/, /about ,", "screenshots": ""}),
        )
        .unwrap();
        assert_eq!(state(&data, "target_pages"), "/, /about");
        call(
            &data,
            "submit_study_report",
            json!({"report": "r", "pages": ""}),
        )
        .unwrap();
        assert_eq!(
            state(&data, "target_pages"),
            "/, /about",
            "an empty study keeps the pages"
        );
    }

    #[test]
    fn a_parity_rejection_builds_the_fix_context_and_counts_iterations() {
        let data = RunData::default();
        call(&data, "reject_parity", json!({"reason": "hero spacing"})).unwrap();
        call(&data, "reject_parity", json!({"reason": "footer colour"})).unwrap();
        assert_eq!(
            state(&data, "fix_context"),
            "Parity verification was rejected (iteration 2): footer colour\nFix exactly these \
             issues before calling the transition tool."
        );
        call(&data, "approve_parity", json!({"summary": "matches"})).unwrap();
        assert_eq!(state(&data, "fix_context"), "");
    }

    #[test]
    fn built_pages_are_normalised_and_unknown_tools_refused() {
        let data = RunData::default();
        call(&data, "submit_pages", json!({"pages_built": "/,/pricing"})).unwrap();
        assert_eq!(state(&data, "pages_built"), "/, /pricing");
        assert!(call(&data, "submit_layout", json!({})).is_err());
        assert!(call(&data, "submit_target", json!({"url": " "})).is_err());
    }
}
