//! Summarizer — produces a human-readable concluding summary of a coding task.
//!
//! Uses `OneShotPlanner` with a summarisation prompt to produce 2–3 plain-text
//! sentences. Falls back to a formatted string if the LLM call fails.

use std::sync::Arc;

use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::planner::{OneShotPlanner, Planner, PlannerContext};
use agtrs_runtime::task::Intent;
use tracing::debug;

use crate::coder::EditSummary;

/// Agent name used for routing and signal identification.
pub const SUMMARIZER_NAME: &str = "summary";

// ── Public API ────────────────────────────────────────────────────────────────

/// Ask the LLM for a brief concluding summary of what was accomplished.
///
/// Falls back to a formatted string from the metadata if the LLM call fails.
pub async fn build_concluding_summary(
    task: &str,
    edit_summary: &EditSummary,
    qa_content: &str,
    approved: bool,
    llm: Arc<dyn LlmProvider>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
) -> String {
    let files_str = if edit_summary.files_changed.is_empty() {
        "(no files recorded)".to_string()
    } else {
        edit_summary.files_changed.join(", ")
    };

    // Strip APPROVED/REJECTED keyword, keep only the explanatory text.
    let qa_note = {
        let c = qa_content.trim();
        let stripped = c
            .trim_start_matches("APPROVED")
            .trim_start_matches("REJECTED")
            .trim();
        if stripped.is_empty() { c } else { stripped }
    };

    let instructions = format!(
        "You are summarising the result of an automated coding task. \
         Write 2–3 short, direct sentences (no bullet points, no markdown). \
         State what was done, which files changed, and the QA outcome.\n\n\
         Task: {task}\n\
         Changes: {desc}\n\
         Files: {files}\n\
         QA: {verdict}\n\
         QA note: {qa_note}",
        desc = edit_summary.description,
        files = files_str,
        verdict = if approved { "APPROVED" } else { "INCOMPLETE" },
    );

    let intent = Intent::from_goal(task).build();
    let ctx = PlannerContext::initial(&intent, vec![]);
    let planner = OneShotPlanner::new(Arc::clone(&llm))
        .with_resolve_ctx(resolve_ctx)
        .with_max_steps(1)
        .with_instructions(&instructions);

    match planner.plan(&ctx).await {
        Ok(plan) if !plan.steps.is_empty() => {
            let text = plan
                .steps
                .iter()
                .map(|s| s.description.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            strip_markdown(&text)
        }
        _ => {
            debug!("summarizer: LLM call failed or returned empty, using fallback");
            let test_str = if edit_summary.tests_passed {
                "Tests passed."
            } else {
                ""
            };
            let qa_line = if !qa_note.is_empty() {
                format!("\n\n{}", strip_markdown(qa_note))
            } else {
                String::new()
            };
            format!(
                "{}{}{}",
                edit_summary.description,
                if test_str.is_empty() { "" } else { " " },
                test_str
            ) + &qa_line
        }
    }
}

// ── Markdown stripper ─────────────────────────────────────────────────────────

/// Strip common markdown formatting from text for plain-terminal display.
///
/// Removes: heading markers (`#`), bold/italic (`**`, `*`, `__`, `_`),
/// inline code backticks, horizontal rules (`---`, `===`), and list bullets
/// (`- `, `* `). Preserves line breaks and text content.
pub fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip pure horizontal rules
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '=' || c == '*' || c == ' ')
            && trimmed.len() >= 3
            && !trimmed.is_empty()
        {
            out.push('\n');
            continue;
        }
        // Strip heading markers (# ## ### etc.)
        let line = if trimmed.starts_with('#') {
            trimmed.trim_start_matches('#').trim()
        } else {
            line
        };
        // Strip list markers (- item, * item, + item)
        let line = if let Some(rest) = line
            .trim_start()
            .strip_prefix("- ")
            .or_else(|| line.trim_start().strip_prefix("* "))
            .or_else(|| line.trim_start().strip_prefix("+ "))
        {
            rest
        } else {
            line
        };
        // Strip inline bold/italic/code markers
        let line = line.replace("**", "").replace("__", "").replace('`', "");
        let line = line.trim_matches('*');
        out.push_str(line);
        out.push('\n');
    }
    // Collapse multiple consecutive blank lines into one
    let mut result = String::with_capacity(out.len());
    let mut blank_count = 0usize;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }
    result.trim().to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_markdown_removes_headings() {
        assert_eq!(strip_markdown("# Hello"), "Hello");
        assert_eq!(strip_markdown("## World"), "World");
    }

    #[test]
    fn strip_markdown_removes_bold() {
        assert_eq!(strip_markdown("**bold** text"), "bold text");
        assert_eq!(strip_markdown("__under__"), "under");
    }

    #[test]
    fn strip_markdown_removes_list_bullets() {
        let input = "- item one\n- item two\n+ item three\n* item four";
        let result = strip_markdown(input);
        assert!(result.contains("item one"));
        assert!(!result.contains("- "));
        assert!(!result.contains("+ "));
    }

    #[test]
    fn strip_markdown_removes_horizontal_rules() {
        let input = "before\n\n---\n\nafter";
        let result = strip_markdown(input);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("---"));
    }

    #[test]
    fn strip_markdown_removes_inline_code() {
        assert_eq!(strip_markdown("use `foo` here"), "use foo here");
    }

    #[test]
    fn strip_markdown_collapses_blank_lines() {
        let input = "a\n\n\n\nb";
        let result = strip_markdown(input);
        assert_eq!(result, "a\n\nb");
    }
}
