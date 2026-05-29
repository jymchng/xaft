//! Planner agent — classifies tasks and routes to the appropriate workflow.
//!
//! Owns the planner system prompt builder, [`PlannerOutput`], [`PlanResult`],
//! and the `parse_plan_result` heuristic parser.

use serde::{Deserialize, Serialize};

/// Agent name used for routing and signal identification.
pub const PLANNER_NAME: &str = "planner";

/// Default maximum LLM turns for the planner.
pub const PLANNER_MAX_TURNS: usize = 8;

// ── PlannerOutput ─────────────────────────────────────────────────────────────

/// Structured output from the smart planner agent.
///
/// The planner uses read-only tools to understand the codebase, then decides:
/// - `"direct_answer"` — task is informational; answer immediately, skip coder.
/// - `"coding_plan"` — task requires file changes; proceed to coder with the plan.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlannerOutput {
    /// `"direct_answer"` or `"coding_plan"`.
    pub task_type: String,
    /// For `"direct_answer"`: the complete response to the user.
    /// For `"coding_plan"`: numbered steps for the coder agent.
    pub content: String,
}

// ── PlanResult ────────────────────────────────────────────────────────────────

/// Result of the planning phase, driving which downstream agents run.
#[derive(Debug, Clone)]
pub enum PlanResult {
    /// Task was answered directly by the planner — coder and QA are skipped.
    DirectAnswer {
        /// Complete answer to return to the user.
        content: String,
    },
    /// Task requires code changes — proceed to coder → QA ↔ fixer.
    CodingPlan {
        /// Numbered step-by-step plan for the coder agent.
        plan_text: String,
    },
}

// ── System prompt ─────────────────────────────────────────────────────────────

/// Build the planner system prompt.
pub fn planner_system_prompt(working_dir: &str) -> String {
    format!(
        "\
You are a smart task analyzer and router for a coding assistant.

WORKING DIRECTORY: {working_dir}
All file paths are relative to this directory.

AVAILABLE TOOLS: list_files, read_file, grep, handoff_to_agent
You do NOT have shell, bash, or command execution access.

WORKFLOW — follow exactly:
1. Call `list_files` to understand the project structure.
2. Call `read_file` on 1–3 files most relevant to the task.
3. Decide:

   INFORMATIONAL task (describe, explain, analyze, summarize, list, show,
   what is, how does, why, what are): write a thorough answer directly in
   your response. Do NOT call any handoff tool.

   CODE-CHANGING task (add, implement, fix, refactor, create, modify,
   delete, update, write, change, rename, remove): call
   `handoff_to_agent` with target_agent=\"coder\" and a numbered
   step-by-step plan as `reason`. The coder will receive your plan.

Do NOT call `handoff_to_agent` for informational tasks — just answer inline.
"
    )
}

// ── parse_plan_result ─────────────────────────────────────────────────────────

/// Parse a raw planner response string into [`PlanResult`].
///
/// Heuristically detects numbered-step plans vs. prose answers when JSON
/// parsing fails.
pub fn parse_plan_result(task: &str, raw: &str) -> PlanResult {
    let trimmed = raw.trim();
    // Try JSON first
    if let Ok(output) = serde_json::from_str::<PlannerOutput>(trimmed) {
        return if output.task_type == "direct_answer" {
            PlanResult::DirectAnswer {
                content: output.content,
            }
        } else {
            PlanResult::CodingPlan {
                plan_text: output.content,
            }
        };
    }
    // Heuristic: if it looks like a numbered plan (starts with "1."), treat as coding
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.starts_with("1.") || first_line.starts_with("1)") {
        PlanResult::CodingPlan {
            plan_text: trimmed.to_string(),
        }
    } else if trimmed.is_empty() || trimmed == task {
        // Unparseable — fall through to coding
        PlanResult::CodingPlan {
            plan_text: task.to_string(),
        }
    } else {
        // Assume prose = direct answer
        PlanResult::DirectAnswer {
            content: trimmed.to_string(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_prompt_contains_working_dir() {
        let prompt = planner_system_prompt("/workspace/project");
        assert!(prompt.contains("/workspace/project"));
        assert!(prompt.contains("INFORMATIONAL"));
        assert!(prompt.contains("CODE-CHANGING"));
    }

    #[test]
    fn parse_numbered_list_is_coding_plan() {
        let raw = "1. Read src/main.rs\n2. Add error handling\n3. Run tests";
        let result = parse_plan_result("add error handling", raw);
        assert!(matches!(result, PlanResult::CodingPlan { .. }));
    }

    #[test]
    fn parse_json_direct_answer() {
        let raw = r#"{"task_type":"direct_answer","content":"This repo does X."}"#;
        let result = parse_plan_result("describe repo", raw);
        if let PlanResult::DirectAnswer { content } = result {
            assert!(content.contains("This repo"));
        } else {
            panic!("expected DirectAnswer");
        }
    }

    #[test]
    fn parse_json_coding_plan() {
        let raw = r#"{"task_type":"coding_plan","content":"1. Modify main.rs"}"#;
        let result = parse_plan_result("add feature", raw);
        assert!(matches!(result, PlanResult::CodingPlan { .. }));
    }

    #[test]
    fn parse_prose_is_direct_answer() {
        let raw = "This repository implements a CLI coding assistant in Rust.";
        let result = parse_plan_result("describe repo", raw);
        assert!(matches!(result, PlanResult::DirectAnswer { .. }));
    }

    #[test]
    fn parse_empty_falls_to_coding_with_task() {
        let result = parse_plan_result("fix the bug", "");
        match result {
            PlanResult::CodingPlan { plan_text } => {
                assert_eq!(plan_text, "fix the bug");
            }
            _ => panic!("expected CodingPlan with task fallback"),
        }
    }

    #[test]
    fn parse_same_as_task_falls_to_coding() {
        let result = parse_plan_result("do something", "do something");
        assert!(matches!(result, PlanResult::CodingPlan { .. }));
    }

    #[test]
    fn planner_output_serialises_roundtrip() {
        let output = PlannerOutput {
            task_type: "coding_plan".into(),
            content: "1. Fix auth\n2. Add tests".into(),
        };
        let json = serde_json::to_string(&output).unwrap();
        let parsed: PlannerOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_type, "coding_plan");
        assert!(parsed.content.contains("Fix auth"));
    }
}
