//! Six built-in agent modes shipped with xaft.

use std::sync::Arc;

use super::registry::ModeRegistry;
use super::{AgentMode, AgentModeBuilder, ModeColour, PostHookFn, ToolFilterFn};

// ── Tool allow-lists ───────────────────────────────────────────────────────────

/// Read-only tools allowed in Plan and Review modes.
pub const PLAN_ALLOWED_TOOLS: &[&str] = &[
    "read_file",
    "read_url",
    "file_exists",
    "file_stat",
    "list_dir",
    "tree",
    "glob",
    "grep_in_file",
    "search_code",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_grep",
];

/// Minimal safe tool set used by Safe mode (subset of PLAN_ALLOWED_TOOLS).
pub const SAFE_ALLOWED_TOOLS: &[&str] = &[
    "read_file",
    "file_exists",
    "file_stat",
    "list_dir",
    "tree",
    "glob",
    "grep_in_file",
    "search_code",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
];

// ── Filter constructors ────────────────────────────────────────────────────────

fn make_filter(allowed: &'static [&'static str]) -> ToolFilterFn {
    Arc::new(move |name: &str| allowed.contains(&name))
}

// ── Post-hook constructor ──────────────────────────────────────────────────────

/// Post-hook for Debug mode: appends a debug footer to agent output.
pub fn debug_post_hook() -> PostHookFn {
    Arc::new(|response: &str| -> String {
        format!(
            "{response}\n\n```\n[xaft:debug] response recorded — check TUI status for live token/cost metrics\n```"
        )
    })
}

// ── Built-in mode constructors ────────────────────────────────────────────────

/// Auto mode — default, no restrictions.
fn mode_auto() -> AgentMode {
    AgentModeBuilder::new("auto", "AUTO")
        .description("Default mode: full capabilities, no restrictions.")
        .colour(ModeColour::Green)
        .build()
}

/// Plan mode — read-only, produces a numbered implementation plan.
fn mode_plan() -> AgentMode {
    AgentModeBuilder::new("plan", "PLAN")
        .description(
            "Read-only planning mode: produces a step-by-step plan without modifying files.",
        )
        .colour(ModeColour::Yellow)
        .system_patch(
            "[MODE: PLAN]\n\
             You are in Plan mode. You MUST NOT write files, edit files, create files, \
             delete files, execute shell commands, commit, push, or call any tool that \
             modifies state. Your only allowed actions are reading files, listing \
             directories, and searching.\n\
             \n\
             Instead of acting, produce a numbered step-by-step implementation plan. \
             For each step include:\n  \
             \u{2022} The action\n  \
             \u{2022} The reason\n  \
             \u{2022} The tool you would use\n\
             \n\
             End your plan with: 'Switch to Auto mode (Shift+Tab) to execute this plan.'",
        )
        .tool_filter(make_filter(PLAN_ALLOWED_TOOLS))
        .build()
}

/// Ask mode — all tools available but agent self-gates with [Y/n] prompts.
fn mode_ask() -> AgentMode {
    AgentModeBuilder::new("ask", "ASK")
        .description("Confirmation mode: agent describes and requests [Y/n] approval before every write or exec.")
        .colour(ModeColour::Cyan)
        .system_patch(
            "[MODE: ASK]\n\
             You are in Ask mode. Before calling any tool that writes files, edits files, \
             deletes files, executes shell commands, commits, or pushes, you MUST first \
             describe what you are about to do and ask the user for confirmation with a \
             [Y/n] prompt. Only proceed if the user responds with 'y', 'yes', or 'Y'.\n\
             \n\
             Format your confirmation request as:\n  \
             > About to: <describe the action>\n  \
             > Reason: <why this is needed>\n  \
             > Proceed? [Y/n]",
        )
        .build()
}

/// Review mode — read-only structured code review.
fn mode_review() -> AgentMode {
    AgentModeBuilder::new("review", "REVIEW")
        .description("Code review mode: produces a structured review (Summary, Issues, Suggestions, Assessment).")
        .colour(ModeColour::Blue)
        .system_patch(
            "[MODE: REVIEW]\n\
             You are in Review mode. You MUST NOT write files, edit files, create files, \
             delete files, execute shell commands, commit, push, or call any tool that \
             modifies state. Your only allowed actions are reading files, listing \
             directories, and searching.\n\
             \n\
             Produce a structured code review with these sections:\n  \
             ## Summary\n  \
             Brief overview of what was reviewed.\n\n  \
             ## Issues\n  \
             Numbered list of bugs, security problems, or correctness issues.\n\n  \
             ## Suggestions\n  \
             Numbered list of improvements (style, performance, maintainability).\n\n  \
             ## Assessment\n  \
             Overall verdict: APPROVED / NEEDS CHANGES / REJECTED.",
        )
        .tool_filter(make_filter(PLAN_ALLOWED_TOOLS))
        .build()
}

/// Safe mode — hard sandbox, minimal tool surface.
fn mode_safe() -> AgentMode {
    AgentModeBuilder::new("safe", "SAFE")
        .description("Hard sandbox: read-only with minimal tool surface. No network, no writes.")
        .colour(ModeColour::Magenta)
        .system_patch(
            "[MODE: SAFE]\n\
             You are in Safe mode \u{2014} a hard sandbox. You MUST NOT write files, edit files, \
             create files, delete files, execute shell commands, access the network, commit, \
             push, or call any tool that modifies state or reaches outside the workspace.\n\
             \n\
             You may only use read-only file system tools. Answer questions by reading \
             source files and explaining the code. If you cannot answer without modifying \
             state, say so explicitly.",
        )
        .tool_filter(make_filter(SAFE_ALLOWED_TOOLS))
        .build()
}

/// Debug mode — full capabilities plus a debug footer on every response.
fn mode_debug() -> AgentMode {
    AgentModeBuilder::new("debug", "DEBUG")
        .description(
            "Debug mode: full capabilities plus per-response debug footer with token/cost info.",
        )
        .colour(ModeColour::Red)
        .post_hook(debug_post_hook())
        .build()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return all 6 built-in modes in canonical order.
pub fn builtin_modes() -> Vec<AgentMode> {
    vec![
        mode_auto(),
        mode_plan(),
        mode_ask(),
        mode_review(),
        mode_safe(),
        mode_debug(),
    ]
}

/// Build a `ModeRegistry` pre-populated with all 6 built-in modes.
pub fn build_default_mode_registry() -> ModeRegistry {
    let mut reg = ModeRegistry::new();
    for mode in builtin_modes() {
        reg.register(mode);
    }
    reg
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_modes_has_six() {
        assert_eq!(builtin_modes().len(), 6);
    }

    #[test]
    fn auto_has_no_filter() {
        let modes = builtin_modes();
        let auto = modes.iter().find(|m| m.name == "auto").unwrap();
        assert!(auto.tool_filter.is_none());
    }

    #[test]
    fn plan_blocks_write_file() {
        let modes = builtin_modes();
        let plan = modes.iter().find(|m| m.name == "plan").unwrap();
        let f = plan.tool_filter.as_ref().unwrap();
        assert!(!f("write_file"));
    }

    #[test]
    fn plan_allows_read_file() {
        let modes = builtin_modes();
        let plan = modes.iter().find(|m| m.name == "plan").unwrap();
        let f = plan.tool_filter.as_ref().unwrap();
        assert!(f("read_file"));
    }

    #[test]
    fn plan_allows_git_status() {
        let modes = builtin_modes();
        let plan = modes.iter().find(|m| m.name == "plan").unwrap();
        let f = plan.tool_filter.as_ref().unwrap();
        assert!(f("git_status"));
    }

    #[test]
    fn plan_system_patch_contains_must_not() {
        let modes = builtin_modes();
        let plan = modes.iter().find(|m| m.name == "plan").unwrap();
        assert!(plan.system_patch.contains("MUST NOT"));
    }

    #[test]
    fn ask_has_no_filter() {
        let modes = builtin_modes();
        let ask = modes.iter().find(|m| m.name == "ask").unwrap();
        assert!(ask.tool_filter.is_none());
    }

    #[test]
    fn review_blocks_bash_exec() {
        let modes = builtin_modes();
        let review = modes.iter().find(|m| m.name == "review").unwrap();
        let f = review.tool_filter.as_ref().unwrap();
        assert!(!f("bash_exec"));
    }

    #[test]
    fn review_allows_git_diff() {
        let modes = builtin_modes();
        let review = modes.iter().find(|m| m.name == "review").unwrap();
        let f = review.tool_filter.as_ref().unwrap();
        assert!(f("git_diff"));
    }

    #[test]
    fn safe_blocks_read_url() {
        let modes = builtin_modes();
        let safe = modes.iter().find(|m| m.name == "safe").unwrap();
        let f = safe.tool_filter.as_ref().unwrap();
        assert!(!f("read_url"));
    }

    #[test]
    fn safe_allows_git_log() {
        let modes = builtin_modes();
        let safe = modes.iter().find(|m| m.name == "safe").unwrap();
        let f = safe.tool_filter.as_ref().unwrap();
        assert!(f("git_log"));
    }

    #[test]
    fn debug_has_post_hook() {
        let modes = builtin_modes();
        let debug = modes.iter().find(|m| m.name == "debug").unwrap();
        assert!(debug.post_hook.is_some());
    }

    #[test]
    fn debug_post_hook_appends_footer() {
        let modes = builtin_modes();
        let debug = modes.iter().find(|m| m.name == "debug").unwrap();
        let hook = debug.post_hook.as_ref().unwrap();
        let result = hook("Agent response here.");
        assert!(result.contains("Agent response here."));
        assert!(result.contains("[xaft:debug]"));
    }
}
