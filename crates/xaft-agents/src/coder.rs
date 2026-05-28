//! Coder agent — edits files to implement a plan.
//!
//! Owns the coder system prompt builder and [`EditSummary`] return type.

use serde::{Deserialize, Serialize};

/// Agent name used for routing and signal identification.
pub const CODER_NAME: &str = "coder";

/// Default maximum LLM turns for the coder.
pub const CODER_MAX_TURNS: usize = 40;

// ── EditSummary ───────────────────────────────────────────────────────────────

/// Structured output from the coder after completing all edits.
///
/// Deserialised from the LLM's JSON response when using `SubagentTool<EditSummary>`.
/// Also used in the standard workflow as a parsed representation of the coder's
/// handoff summary.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditSummary {
    /// Workspace-relative paths of every file changed.
    pub files_changed: Vec<String>,
    /// Brief human-readable description of what was done.
    pub description: String,
    /// Whether the agent ran and passed tests.
    #[serde(default)]
    pub tests_passed: bool,
    /// Optional notes.
    #[serde(default)]
    pub notes: String,
}

// ── System prompt ─────────────────────────────────────────────────────────────

/// Build the coder system prompt.
///
/// `plan_text` is the planner's step-by-step plan to embed in the prompt.
/// `working_dir` is the workspace root displayed to the LLM.
pub fn coder_system_prompt(plan_text: &str, working_dir: &str) -> String {
    let plan_section = if plan_text.is_empty() {
        String::new()
    } else {
        format!("PLAN — execute these steps in order:\n{plan_text}\n\n")
    };
    format!(
        "{plan_section}\
You are an expert software engineer. Edit files using the provided tools.

WORKING DIRECTORY: {working_dir}
All file paths are relative to this directory. Use relative paths only. Do NOT use `cd`.

WORKFLOW — follow this order exactly:
1. Read the plan from your incoming message and understand what needs to change.
2. Call `list_files` to discover what files exist.
3. Call `read_file` to read each relevant file before editing.
4. For targeted edits call `edit_file` with {{\"path\": \"<f>\", \"old_content\": \"<exact>\", \"new_content\": \"<replacement>\"}}.
5. To create or fully rewrite a file call `write_file` with {{\"path\": \"<f>\", \"content\": \"<full content>\"}}.
6. Call `bash_exec` to verify changes. Fix any failures before proceeding.
7. When ALL changes are done:
   a. Write a brief text summary: \"Changed: [file1, file2]. [One sentence description].\"
   b. Then call `handoff_to_agent` with target_agent=\"qa\" and the same text as `reason`.

RULES:
- Always read a file before editing it
- Make minimal targeted changes
- Supply ALL required fields in every tool call
- After calling handoff_to_agent, stop immediately — do not call any more tools
"
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_summary_serialises_roundtrip() {
        let summary = EditSummary {
            files_changed: vec!["src/main.rs".into(), "src/lib.rs".into()],
            description: "Added error handling".into(),
            tests_passed: true,
            notes: "all green".into(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: EditSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.files_changed.len(), 2);
        assert!(parsed.tests_passed);
    }

    #[test]
    fn edit_summary_defaults_on_missing_fields() {
        let json = r#"{"files_changed":[],"description":"done"}"#;
        let parsed: EditSummary = serde_json::from_str(json).unwrap();
        assert!(!parsed.tests_passed);
        assert!(parsed.notes.is_empty());
    }

    #[test]
    fn system_prompt_includes_working_dir() {
        let prompt = coder_system_prompt("plan steps", "/workspace/project");
        assert!(prompt.contains("/workspace/project"));
        assert!(prompt.contains("plan steps"));
    }

    #[test]
    fn system_prompt_empty_plan_no_plan_section() {
        let prompt = coder_system_prompt("", "/workspace");
        assert!(!prompt.contains("PLAN —"));
        assert!(prompt.contains("expert software engineer"));
    }
}
