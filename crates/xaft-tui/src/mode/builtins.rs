//! Built-in agent modes shipped with xaft.
//!
//! Five selectable modes — `auto` (agenthicc's Yolo), `plan`, `safe`, and the
//! legacy `ask` / `review` (shadowed by the agenthicc alias table, see
//! parity-matrix §3c) — plus agenthicc's internal `replay` mode, which no user
//! can select. agenthicc registers no `Debug` identity and reserves the name,
//! so xaft does not ship one either (`registry.rs` refuses it).

use std::sync::Arc;

use super::registry::ModeRegistry;
use super::{AgentMode, AgentModeBuilder, ModeColour};

// ── Tool allow-lists ───────────────────────────────────────────────────────────

/// The read core Plan and Review always keep, by name — documentation only:
/// Plan filters by capability ([`plan_allows`]), so every read, search,
/// git-read and workflow-control tool is kept, not just these.
pub const PLAN_ALLOWED_TOOLS: &[&str] = &[
    "read_file",
    "file_exists",
    "file_stat",
    "tree",
    "glob",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_grep",
];

/// The read-only tool set xaft's Safe mode used to be limited to.
///
/// Kept as the documented read core: every name here is also in
/// [`SAFE_DIRECT_TOOLS`], so Safe still runs these without asking. Safe no
/// longer *filters* to this list — see [`mode_safe`].
pub const SAFE_ALLOWED_TOOLS: &[&str] = &[
    "read_file",
    "file_exists",
    "file_stat",
    "tree",
    "glob",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
];

/// Tools Safe mode runs **without** asking: agenthicc's `READ`, `SEARCH`,
/// `GIT_READ` and `CONTROL` capabilities, by xaft tool name.
///
/// Everything else — writes, edits, deletes, git writes, shell and code
/// execution, the terminal tools (agenthicc tags those `EXECUTE`), network
/// (`read_url`), memory writes, and any tool `capability_of` does not know
/// (agenthicc's fail-closed `UNDECLARED`) — needs explicit approval in Safe.
/// The decision is made by capability ([`safe_requires_approval`]); this
/// list names the direct tools for reference and is checked against it.
pub const SAFE_DIRECT_TOOLS: &[&str] = &[
    // read / search
    "read_file",
    "read_lines",
    "batch_read",
    "file_exists",
    "file_stat",
    "checksum_file",
    "diff_files",
    "list_files",
    "tree",
    "glob",
    "grep",
    "search_files",
    "recall",
    "summarize_memory",
    "mcp_connection_guide",
    // git read
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_grep",
    "git_stash_list",
    "git_remote",
    "git_tag",
    // workflow control
    "ask_user",
    "handoff_to_agent",
    "request_fix",
    "explore_repository",
];

/// Whether Safe mode requires explicit approval before `tool` runs:
/// agenthicc's rule, by capability — a tool whose capability is restricted
/// (write, git write, execute, network, memory, undeclared) asks; reads,
/// searches, git reads and workflow control (`ask_user`, transitions,
/// handoffs) run directly. [`SAFE_DIRECT_TOOLS`] lists the direct ones by
/// name for reference.
pub fn safe_requires_approval(tool: &str) -> bool {
    xaft_tools::capabilities::capability_of(tool).is_restricted()
}

/// Whether Plan mode keeps `tool`: agenthicc's Plan hard-blocks the
/// restricted capabilities and nothing else, so reads, searches, git reads
/// and workflow control (`ask_user` included) stay available.
pub fn plan_allows(tool: &str) -> bool {
    !xaft_tools::capabilities::capability_of(tool).is_restricted()
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
        .tool_filter(Arc::new(plan_allows))
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
        .tool_filter(Arc::new(plan_allows))
        .build()
}

/// Safe mode — agenthicc's gate-and-ask Safe (parity §3 row 1).
///
/// Reads, searches and git reads ([`SAFE_DIRECT_TOOLS`]) run directly; every
/// other tool stays available but needs **explicit** approval before each
/// call. That is a runtime property, not an instruction: the approval filter
/// travels in `RunRequest::mode_approval_filter`, the runtime wraps each
/// matching tool so it requires confirmation, and the TUI never auto-approves
/// it. agenthicc's Safe hard-blocks nothing, so neither does this mode.
fn mode_safe() -> AgentMode {
    AgentModeBuilder::new("safe", "SAFE")
        .description("Side-effecting actions require explicit approval; reads are allowed.")
        .colour(ModeColour::Magenta)
        .system_patch(
            "## SAFE MODE\n\
             Side-effecting tools require explicit user approval before execution. \
             Read, search, and git-read tools may be used directly.",
        )
        .approval_filter(Arc::new(safe_requires_approval))
        .build()
}

/// Replay — agenthicc's internal mode for replaying a session (§3 row 7).
///
/// Registered non-selectable: it is never in the Shift+Tab cycle, never in
/// the `/mode` listing, and `/mode replay` is refused as an unknown mode.
/// Every tool is blocked. `/replay` enters it through
/// `ModeManager::set_internal` for the duration of the replay and restores the
/// previous mode afterwards.
fn mode_replay() -> AgentMode {
    AgentModeBuilder::new("replay", "REPLAY")
        .description("Internal replay state; all tool capabilities are blocked.")
        .colour(ModeColour::White)
        .tool_filter(Arc::new(|_: &str| false))
        .internal()
        .build()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return every built-in mode in canonical order: the five selectable modes,
/// then the internal `replay` mode.
pub fn builtin_modes() -> Vec<AgentMode> {
    vec![
        mode_auto(),
        mode_plan(),
        mode_ask(),
        mode_review(),
        mode_safe(),
        mode_replay(),
    ]
}

/// Build a `ModeRegistry` pre-populated with every built-in mode.
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
    fn builtin_modes_are_five_selectable_plus_internal_replay() {
        let modes = builtin_modes();
        let selectable: Vec<&str> = modes
            .iter()
            .filter(|m| m.selectable)
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(selectable, vec!["auto", "plan", "ask", "review", "safe"]);
        let internal: Vec<&str> = modes
            .iter()
            .filter(|m| !m.selectable)
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(internal, vec!["replay"]);
        assert!(
            !modes.iter().any(|m| m.name == "debug"),
            "agenthicc registers no Debug identity"
        );
    }

    #[test]
    fn auto_has_no_filter() {
        let modes = builtin_modes();
        let auto = modes.iter().find(|m| m.name == "auto").unwrap();
        assert!(auto.tool_filter.is_none());
    }

    /// Plan is agenthicc's: restricted capabilities are blocked, workflow
    /// control is kept — the agent can still ask the user while it plans.
    #[test]
    fn plan_keeps_ask_user_and_blocks_network_and_execution() {
        let modes = builtin_modes();
        let plan = modes.iter().find(|m| m.name == "plan").unwrap();
        let f = plan.tool_filter.as_ref().unwrap();
        assert!(f("ask_user"));
        assert!(f("git_tag"), "listing tags is a git read");
        for blocked in [
            "read_url",
            "bash_exec",
            "git_push",
            "remember",
            "mcp__srv__x",
        ] {
            assert!(!f(blocked), "{blocked} must be blocked in Plan");
        }
    }

    /// Every tool these reference lists name is a real, classified tool, and
    /// the capability rules agree with the lists.
    #[test]
    fn the_reference_lists_name_classified_tools() {
        use xaft_tools::capabilities::{ToolCapability, capability_of};
        for tool in SAFE_DIRECT_TOOLS
            .iter()
            .chain(SAFE_ALLOWED_TOOLS)
            .chain(PLAN_ALLOWED_TOOLS)
        {
            assert_ne!(
                capability_of(tool),
                ToolCapability::Undeclared,
                "{tool} is not a known tool"
            );
            assert!(plan_allows(tool) && !safe_requires_approval(tool), "{tool}");
        }
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

    /// agenthicc's Safe hard-blocks nothing: every tool stays available.
    #[test]
    fn safe_filters_no_tool_out() {
        let modes = builtin_modes();
        let safe = modes.iter().find(|m| m.name == "safe").unwrap();
        assert!(safe.tool_filter.is_none());
    }

    /// Network, writes, execution and git writes need explicit approval.
    #[test]
    fn safe_requires_approval_for_side_effects() {
        let modes = builtin_modes();
        let safe = modes.iter().find(|m| m.name == "safe").unwrap();
        let needs = safe.approval_filter.as_ref().unwrap();
        for tool in [
            "read_url",
            "write_file",
            "edit_file",
            "delete_file",
            "bash_exec",
            "run_command",
            "git_commit_staged",
            "git_push",
            "stop_terminal",
            "remember",
            "some_mcp_tool",
        ] {
            assert!(needs(tool), "{tool} must need approval in Safe");
        }
    }

    /// Reads, searches and git reads run directly.
    #[test]
    fn safe_runs_reads_directly() {
        let modes = builtin_modes();
        let safe = modes.iter().find(|m| m.name == "safe").unwrap();
        let needs = safe.approval_filter.as_ref().unwrap();
        for tool in SAFE_ALLOWED_TOOLS.iter().chain(SAFE_DIRECT_TOOLS) {
            assert!(!needs(tool), "{tool} runs directly in Safe");
        }
    }

    #[test]
    fn replay_is_internal_and_blocks_every_tool() {
        let modes = builtin_modes();
        let replay = modes.iter().find(|m| m.name == "replay").unwrap();
        assert!(!replay.selectable);
        let allowed = replay.tool_filter.as_ref().unwrap();
        for tool in ["read_file", "git_status", "write_file", "bash_exec"] {
            assert!(!allowed(tool), "{tool} must be blocked during replay");
        }
    }
}
