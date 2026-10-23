//! The nine typed subagent roles (agenthicc's `subagents/types.py`).
//!
//! A [`SubagentTypeSpec`] is declarative: a stable role name, a system
//! prompt, a turn budget, a timeout default and a tool **ceiling**. The pool
//! intersects that ceiling with the tools the parent turn can actually see —
//! after the session mode and the phase have filtered them — so registering a
//! role never grants a capability the parent does not have.
//!
//! The prompts are agenthicc's, verbatim. The allow-lists are agenthicc's,
//! translated to xaft's tool names by the parity matrix's §6 mapping
//! (`list_directory` → `list_files`, `grep_files` → `search_files`,
//! `grep_file` → `grep`, `get_file_info` → `file_stat`, `append_file` →
//! `append_to_file`, `run_bash`/`shell` → `bash_exec`). Two additions are
//! deliberate: a role that may `write_file` may also `edit_file`, xaft's
//! primary in-place edit tool, and a role that may list directories may also
//! use `glob` and `tree`, the two listing tools agenthicc folds into
//! `list_directory`/`search_files`.

use std::collections::BTreeSet;

/// The default per-worker wall-clock budget of one `spawn_subagents` call:
/// one hour (agenthicc's `DEFAULT_SUBAGENT_TIMEOUT_S`).
pub const DEFAULT_SUBAGENT_TIMEOUT_S: f64 = 3_600.0;

/// Spellings models use for a role, mapped to its canonical name
/// (agenthicc's `_SUBAGENT_TYPE_ALIASES`).
pub const SUBAGENT_TYPE_ALIASES: &[(&str, &str)] = &[("research", "researcher")];

/// One role.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentTypeSpec {
    /// The role name used in `spawn_subagents(tasks=[{"type": …}])`.
    pub name: String,
    /// The tool ceiling: tool names this role may call.
    pub allowed_tools: BTreeSet<String>,
    /// Maximum model turns per worker.
    pub max_turns: usize,
    /// The role's system prompt.
    pub system_prompt: String,
    /// The role's own timeout, used when a call does not set one.
    pub max_turn_time_s: f64,
}

impl SubagentTypeSpec {
    fn builtin(
        name: &str,
        tools: &[&str],
        max_turns: usize,
        system_prompt: &str,
        max_turn_time_s: f64,
    ) -> Self {
        Self {
            name: name.into(),
            allowed_tools: tools.iter().map(|t| (*t).to_string()).collect(),
            max_turns,
            system_prompt: system_prompt.into(),
            max_turn_time_s,
        }
    }
}

const EXPLORER_PROMPT: &str = "You are a read-only codebase explorer. \
Your job is to investigate the repository and gather facts. \
You may read files, list directories, search, grep, and inspect git history. \
You must NOT write, edit, or delete any files. \
When you have gathered enough information, write a clear Markdown summary of your findings \
with specific file paths and line numbers where relevant. \
End your response with a concise one-paragraph synthesis.";

const PLANNER_PROMPT: &str = "You are an implementation planner. \
Your job is to read the codebase and produce a clear, numbered implementation plan \
for the task you are given. \
You may read files and search the codebase. You must NOT modify any files. \
End your response with a numbered list of concrete implementation steps, \
each scoped to a specific file or function.";

const IMPLEMENTER_PROMPT: &str = "You are a focused implementer. \
Your job is to carry out a specific, scoped code change. \
Read the relevant files, then use an available write or patch tool to make the change; \
a prose description of a change you would make is not completion. \
After writing, reread the target or run a focused check so you can report evidence that \
the change actually exists. \
Stay strictly within the files mentioned in your task — do not refactor unrelated code. \
End your response with a brief summary of exactly what you changed and why.";

const EXECUTOR_PROMPT: &str = "You are a general execution agent. \
Your job is to carry out the requested implementation or build task end to end. \
Inspect the workspace before changing it, use the available filesystem and command tools, \
and verify the result rather than claiming success from an unexecuted plan. \
Keep changes scoped to the task and report commands, important output, and any remaining \
failure clearly. End your response with a concise completion summary.";

const TESTER_PROMPT: &str = "You are a test engineer. \
Your job is to write or run tests for the component described in your task. \
Write clear, well-named tests, run them, and report the outcome. \
End your response with a summary of tests written, passed, and failed, \
and any failures with their error messages.";

const REVIEWER_PROMPT: &str = "You are a code reviewer. \
Your job is to review the code or change described in your task for correctness, \
clarity, security issues, and potential bugs. \
Read the relevant files. Do not modify anything. \
End your response with a clear verdict — APPROVED or NEEDS CHANGES — \
followed by a bulleted list of issues found (or 'No issues found').";

const DOCUMENTER_PROMPT: &str = "You are a documentation writer. \
Your job is to write or update documentation for the module, function, or API \
described in your task. \
Read the source code to understand what to document, then write accurate docs. \
If the task asks you to return full, rewritten, or verbatim content, that content \
must be present in your final response in full; do not substitute a summary of the \
read_file calls. Otherwise, end your response with a brief summary of what you \
documented and which files changed.";

const VERIFIER_PROMPT: &str = "You are an adversarial verifier. \
Your job is to check whether a specific requirement, invariant, or assertion holds \
in the codebase. Actively look for counter-evidence. \
Run tests if relevant. Read files and grep for patterns. \
End your response with a clear verdict — VERIFIED or NOT VERIFIED — \
followed by the evidence you found for and against.";

const RESEARCHER_PROMPT: &str = "You are a technical researcher. \
Your job is to find answers to specific technical questions. \
Search local files and documentation first. \
End your response with a direct answer to the question and any relevant sources.";

const EXPLORER_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "glob",
    "tree",
    "search_files",
    "grep",
    "git_log",
    "git_show",
    "git_blame",
    "git_grep",
    "file_exists",
    "file_stat",
    "read_lines",
];

const PLANNER_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "glob",
    "tree",
    "search_files",
    "grep",
    "read_lines",
    "file_exists",
];

const IMPLEMENTER_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "patch_file",
    "append_to_file",
    "list_files",
    "glob",
    "tree",
    "search_files",
    "grep",
    "run_python_expr",
    "read_lines",
    "file_exists",
    "file_stat",
];

/// `executor` = the implementer's tools plus the command tools a build needs.
const EXECUTOR_EXTRA_TOOLS: &[&str] = &["bash_exec", "run_command", "run_python", "run_tests"];

const TESTER_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "patch_file",
    "run_tests",
    "bash_exec",
    "run_python_expr",
    "list_files",
    "glob",
    "tree",
    "search_files",
    "read_lines",
];

const REVIEWER_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "glob",
    "tree",
    "search_files",
    "grep",
    "git_diff",
    "git_log",
    "run_python_expr",
    "read_lines",
    "file_exists",
];

const DOCUMENTER_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "patch_file",
    "search_files",
    "list_files",
    "glob",
    "tree",
    "read_lines",
];

const VERIFIER_TOOLS: &[&str] = &[
    "read_file",
    "search_files",
    "grep",
    "run_tests",
    "run_python_expr",
    "git_diff",
    "git_log",
    "read_lines",
    "file_exists",
];

const RESEARCHER_TOOLS: &[&str] = &[
    "read_file",
    "read_lines",
    "search_files",
    "list_files",
    "glob",
    "tree",
];

/// Maps role names to specs, in registration order.
#[derive(Debug, Clone, Default)]
pub struct SubagentTypeRegistry {
    types: Vec<SubagentTypeSpec>,
}

impl SubagentTypeRegistry {
    /// The nine built-in roles, in agenthicc's order: explorer, planner,
    /// implementer, executor, tester, reviewer, documenter, verifier,
    /// researcher.
    pub fn builtin() -> Self {
        let executor_tools: Vec<&str> = IMPLEMENTER_TOOLS
            .iter()
            .chain(EXECUTOR_EXTRA_TOOLS)
            .copied()
            .collect();
        let mut registry = Self::default();
        for spec in [
            SubagentTypeSpec::builtin(
                "explorer",
                EXPLORER_TOOLS,
                15,
                EXPLORER_PROMPT,
                DEFAULT_SUBAGENT_TIMEOUT_S,
            ),
            SubagentTypeSpec::builtin(
                "planner",
                PLANNER_TOOLS,
                10,
                PLANNER_PROMPT,
                DEFAULT_SUBAGENT_TIMEOUT_S,
            ),
            SubagentTypeSpec::builtin(
                "implementer",
                IMPLEMENTER_TOOLS,
                20,
                IMPLEMENTER_PROMPT,
                180.0,
            ),
            SubagentTypeSpec::builtin("executor", &executor_tools, 24, EXECUTOR_PROMPT, 300.0),
            SubagentTypeSpec::builtin("tester", TESTER_TOOLS, 20, TESTER_PROMPT, 180.0),
            SubagentTypeSpec::builtin(
                "reviewer",
                REVIEWER_TOOLS,
                10,
                REVIEWER_PROMPT,
                DEFAULT_SUBAGENT_TIMEOUT_S,
            ),
            SubagentTypeSpec::builtin("documenter", DOCUMENTER_TOOLS, 15, DOCUMENTER_PROMPT, 150.0),
            SubagentTypeSpec::builtin("verifier", VERIFIER_TOOLS, 12, VERIFIER_PROMPT, 150.0),
            SubagentTypeSpec::builtin(
                "researcher",
                RESEARCHER_TOOLS,
                8,
                RESEARCHER_PROMPT,
                DEFAULT_SUBAGENT_TIMEOUT_S,
            ),
        ] {
            registry.register(spec);
        }
        registry
    }

    /// Register or replace a role.
    pub fn register(&mut self, spec: SubagentTypeSpec) {
        match self.types.iter_mut().find(|t| t.name == spec.name) {
            Some(existing) => *existing = spec,
            None => self.types.push(spec),
        }
    }

    /// The spec for `name`, or `None`.
    pub fn get(&self, name: &str) -> Option<&SubagentTypeSpec> {
        self.types.iter().find(|t| t.name == name)
    }

    /// Whether `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Registered role names, in registration order.
    pub fn names(&self) -> Vec<String> {
        self.types.iter().map(|t| t.name.clone()).collect()
    }

    /// The canonical name a caller's spelling resolves to.
    pub fn resolve<'a>(&self, name: &'a str) -> &'a str {
        SUBAGENT_TYPE_ALIASES
            .iter()
            .find(|(alias, canonical)| *alias == name && self.contains(canonical))
            .map(|(_, canonical)| *canonical)
            .unwrap_or(name)
    }

    /// Canonical names plus the aliases that resolve in this registry
    /// (agenthicc's `_known_type_names`), for errors and the tool schema.
    pub fn known_type_names(&self) -> Vec<String> {
        let mut names = self.names();
        for (alias, canonical) in SUBAGENT_TYPE_ALIASES {
            if self.contains(canonical) && !names.iter().any(|n| n == alias) {
                names.push((*alias).to_string());
            }
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nine_builtin_roles_in_agenthiccs_order() {
        let registry = SubagentTypeRegistry::builtin();
        assert_eq!(
            registry.names(),
            [
                "explorer",
                "planner",
                "implementer",
                "executor",
                "tester",
                "reviewer",
                "documenter",
                "verifier",
                "researcher"
            ]
        );
        let turns: Vec<usize> = registry.types.iter().map(|t| t.max_turns).collect();
        assert_eq!(turns, [15, 10, 20, 24, 20, 10, 15, 12, 8]);
    }

    #[test]
    fn read_only_roles_get_no_mutation_or_command_tool() {
        let registry = SubagentTypeRegistry::builtin();
        for role in ["explorer", "planner", "reviewer", "verifier", "researcher"] {
            let tools = &registry.get(role).unwrap().allowed_tools;
            for forbidden in ["write_file", "edit_file", "patch_file", "delete_file"] {
                assert!(!tools.contains(forbidden), "{role} may not {forbidden}");
            }
        }
        for role in ["explorer", "planner", "researcher", "reviewer"] {
            let tools = &registry.get(role).unwrap().allowed_tools;
            assert!(!tools.contains("bash_exec") && !tools.contains("run_command"));
        }
    }

    #[test]
    fn executor_is_the_implementer_plus_commands() {
        let registry = SubagentTypeRegistry::builtin();
        let implementer = &registry.get("implementer").unwrap().allowed_tools;
        let executor = &registry.get("executor").unwrap().allowed_tools;
        assert!(implementer.is_subset(executor));
        for command in EXECUTOR_EXTRA_TOOLS {
            assert!(executor.contains(*command));
        }
    }

    #[test]
    fn research_is_an_alias_for_researcher() {
        let registry = SubagentTypeRegistry::builtin();
        assert_eq!(registry.resolve("research"), "researcher");
        assert_eq!(registry.resolve("explorer"), "explorer");
        assert_eq!(registry.resolve("bogus"), "bogus");
        assert_eq!(registry.known_type_names().last().unwrap(), "research");
    }

    #[test]
    fn every_allowed_tool_is_a_tool_xaft_classifies() {
        use xaft_tools::capabilities::{ToolCapability, capability_of};
        let registry = SubagentTypeRegistry::builtin();
        for spec in &registry.types {
            for tool in &spec.allowed_tools {
                assert_ne!(
                    capability_of(tool),
                    ToolCapability::Undeclared,
                    "{}: '{tool}' is not an xaft tool name",
                    spec.name
                );
            }
        }
    }
}
