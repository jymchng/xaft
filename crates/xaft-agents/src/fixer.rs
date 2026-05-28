//! Fixer agent — addresses issues reported by the QA agent.
//!
//! Owns the fixer system prompt builder and [`FixSummary`].

use serde::{Deserialize, Serialize};

/// Agent name used for routing and signal identification.
pub const FIXER_NAME: &str = "fixer";

/// Default maximum LLM turns for the fixer.
pub const FIXER_MAX_TURNS: usize = 25;

// ── FixSummary ────────────────────────────────────────────────────────────────

/// Structured output from the fixer after addressing QA feedback.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixSummary {
    /// Workspace-relative paths of files that were fixed.
    pub files_fixed: Vec<String>,
    /// Brief description of what was fixed.
    pub description: String,
}

// ── System prompt ─────────────────────────────────────────────────────────────

/// Build the fixer system prompt.
pub fn fixer_system_prompt(task: &str, working_dir: &str) -> String {
    format!(
        "\
You are a bug fixer working on this task: {task}
WORKING DIRECTORY: {working_dir}
Use relative paths. Do NOT use `cd`.

INSTRUCTIONS:
1. Read the issues described in your incoming message.
2. Call `list_files` to see all files in the workspace.
3. Call `read_file` for each file that needs fixing.
4. For each file to fix, call `write_file` with the COMPLETE corrected content.
   Fix ALL reported issues. Write the full file — not just the changed lines.
5. When done, call `handoff_to_agent` with target_agent=\"qa\" and a brief summary
   of what you fixed. The QA agent will review your changes.

Do NOT output file content as plain text — all changes must go through write_file.
You MUST call handoff_to_agent(\"qa\", ...) when done — do not just output text."
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixer_prompt_contains_task_and_dir() {
        let prompt = fixer_system_prompt("fix auth bug", "/workspace/app");
        assert!(prompt.contains("fix auth bug"));
        assert!(prompt.contains("/workspace/app"));
        assert!(prompt.contains("write_file"));
    }

    #[test]
    fn fix_summary_serialises_roundtrip() {
        let summary = FixSummary {
            files_fixed: vec!["src/auth.rs".into()],
            description: "Fixed missing error handling".into(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: FixSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.files_fixed.len(), 1);
        assert!(parsed.description.contains("error handling"));
    }
}
