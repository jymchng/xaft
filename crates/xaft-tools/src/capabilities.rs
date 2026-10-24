//! Tool capabilities — what kind of side effect a tool can have.
//!
//! agenthicc tags every tool with `@tool_read`, `@tool_write`, … and filters
//! a workflow phase's tools by `PhaseSpec.allowed_capabilities` intersected
//! with the mode's blocked set (`agenthicc/tools/capabilities.py`). xaft's
//! tools carry no such metadata, so the classification lives here, keyed by
//! the registered tool name — one table, shared by the phased workflow engine
//! and anything else that needs to reason about side effects.
//!
//! A name this table does not know is [`ToolCapability::Undeclared`], the
//! same fail-closed marker agenthicc uses for a tool without metadata: a
//! phase that restricts capabilities never receives an unknown tool.
//!
//! ```
//! use xaft_tools::capabilities::{ToolCapability, capability_of};
//!
//! assert_eq!(capability_of("read_file"), ToolCapability::Read);
//! assert_eq!(capability_of("git_commit_staged"), ToolCapability::GitWrite);
//! assert_eq!(capability_of("mcp__github__create_issue"), ToolCapability::Network);
//! assert_eq!(capability_of("something_new"), ToolCapability::Undeclared);
//! assert!(ToolCapability::Read.is_read_only());
//! assert!(!ToolCapability::Execute.is_read_only());
//! ```

/// The side-effect class of a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ToolCapability {
    /// Reads files or data; no persistent side effects.
    Read,
    /// Searches content without state changes.
    Search,
    /// Creates, modifies or deletes files.
    Write,
    /// Runs shell commands or arbitrary code.
    Execute,
    /// Reads git history, diffs, status or blame.
    GitRead,
    /// Modifies git state.
    GitWrite,
    /// Makes outbound network calls.
    Network,
    /// Writes to long-term memory.
    Memory,
    /// Advances a workflow or session state machine.
    Control,
    /// No metadata — treated as fail-closed by capability filters.
    Undeclared,
}

impl ToolCapability {
    /// Every capability, in declaration order.
    pub const ALL: [ToolCapability; 10] = [
        Self::Read,
        Self::Search,
        Self::Write,
        Self::Execute,
        Self::GitRead,
        Self::GitWrite,
        Self::Network,
        Self::Memory,
        Self::Control,
        Self::Undeclared,
    ];

    /// The canonical lower-case name, as written in workflow definitions.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Search => "search",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::GitRead => "git_read",
            Self::GitWrite => "git_write",
            Self::Network => "network",
            Self::Memory => "memory",
            Self::Control => "control",
            Self::Undeclared => "undeclared",
        }
    }

    /// Parse a capability name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == name.trim().to_ascii_lowercase())
    }

    /// One-line explanation, for authoring tools and help text.
    pub fn description(self) -> &'static str {
        match self {
            Self::Read => "reads files or data with no persistent side effects",
            Self::Search => "searches content without changing state",
            Self::Write => "creates, modifies or deletes files",
            Self::Execute => "runs shell commands or arbitrary code",
            Self::GitRead => "reads git history, diffs, status or blame",
            Self::GitWrite => "modifies git state (add, commit, checkout, push, …)",
            Self::Network => "makes outbound network calls (MCP servers, browsers, mail, …)",
            Self::Memory => "writes to long-term memory",
            Self::Control => "advances a workflow or session state machine",
            Self::Undeclared => "unknown tool; never granted by a capability allowlist",
        }
    }

    /// Whether the capability has no side effects outside the conversation:
    /// `read`, `search` and `git_read` — agenthicc's `READ_CAPS`.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Read | Self::Search | Self::GitRead)
    }

    /// Whether the capability is one agenthicc's modes restrict — Plan
    /// hard-blocks it, Safe asks before every call: agenthicc's
    /// `_restricted_capabilities()` (`write`, `git_write`, `execute`,
    /// `network`, `undeclared`) plus xaft's own `memory`, which writes
    /// long-term memory and is a `write` in agenthicc's vocabulary (agenthicc
    /// has no memory capability). `control` is not restricted: asking the user
    /// a question or advancing a workflow is never a side effect on the
    /// project.
    pub fn is_restricted(self) -> bool {
        matches!(
            self,
            Self::Write
                | Self::GitWrite
                | Self::Execute
                | Self::Network
                | Self::Memory
                | Self::Undeclared
        )
    }
}

impl std::fmt::Display for ToolCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// agenthicc's `READ_CAPS`: the default allowlist of read-only roles.
pub const READ_CAPS: [ToolCapability; 3] = [
    ToolCapability::Read,
    ToolCapability::Search,
    ToolCapability::GitRead,
];

/// Classify a tool by its registered name.
///
/// Built-in names come from the static table ([`builtin_capability_of`]); a
/// persistent custom tool (`.xaft/tools/<name>.toml`) is classified by the
/// capability its file declares, recorded when it was loaded
/// ([`declare_custom_capability`]).
pub fn capability_of(tool_name: &str) -> ToolCapability {
    let builtin = builtin_capability_of(tool_name);
    if builtin != ToolCapability::Undeclared {
        return builtin;
    }
    declared()
        .read()
        .ok()
        .and_then(|map| map.get(tool_name).copied().flatten())
        .unwrap_or(ToolCapability::Undeclared)
}

/// Custom tool capabilities declared in this process; `None` marks a name
/// declared twice with different capabilities.
fn declared()
-> &'static std::sync::RwLock<std::collections::HashMap<String, Option<ToolCapability>>> {
    static DECLARED: std::sync::OnceLock<
        std::sync::RwLock<std::collections::HashMap<String, Option<ToolCapability>>>,
    > = std::sync::OnceLock::new();
    DECLARED.get_or_init(Default::default)
}

/// Record the capability a persistent custom tool declares, so capability
/// filters treat it like a built-in tool of that class.
///
/// A built-in name is never reclassified (the loader refuses such tools
/// anyway). A name declared again with a *different* capability in the same
/// process — two projects shipping a same-named tool, or a file edited while
/// a session runs — becomes [`ToolCapability::Undeclared`], the fail-closed
/// class no capability allowlist grants.
pub fn declare_custom_capability(tool_name: &str, capability: ToolCapability) {
    if builtin_capability_of(tool_name) != ToolCapability::Undeclared {
        return;
    }
    let Ok(mut map) = declared().write() else {
        return;
    };
    let entry = map.entry(tool_name.to_string()).or_insert(Some(capability));
    if *entry != Some(capability) {
        *entry = None;
    }
}

/// Classify a built-in tool by its registered name (the static table only).
pub fn builtin_capability_of(tool_name: &str) -> ToolCapability {
    use ToolCapability::*;
    match tool_name {
        // ── File reads ──
        "read_file"
        | "read_lines"
        | "batch_read"
        | "file_exists"
        | "file_stat"
        | "checksum_file"
        | "diff_files"
        | "list_files"
        | "tree"
        | "inspect_terminal"
        | "mcp_connection_guide"
        | "explore_repository"
        | "recall"
        | "summarize_memory"
        | "list_goals"
        | "get_state"
        | "list_state"
        | "list_artifacts"
        | "lookup_component"
        | "list_components"
        // agenthicc's self-inspection tools (`tool_read`), over xaft's corpus
        | "list_xaft_docs"
        | "read_xaft_doc"
        | "inspect_xaft_source"
        // make_book's reference and make_xaft_tool's schema/listing.
        | "list_build_book_reference"
        | "read_build_book_reference"
        | "describe_custom_tool_schema"
        | "list_custom_tools" => Read,
        // ── Searches ──
        "glob" | "grep" | "search_files" | "search_xaft_docs" | "search_xaft_source" => Search,
        // ── File writes ──
        "write_file"
        | "edit_file"
        | "patch_file"
        | "append_to_file"
        | "create_directory"
        | "delete_file"
        | "move_file"
        | "copy_file"
        | "batch_copy"
        | "batch_delete"
        | "batch_move"
        | "batch_write"
        | "touch_file"
        | "truncate_file"
        | "remove_directory"
        | "record_reconstruct_screenshot"
        | "create_build_book" => Write,
        // ── Execution ──
        "bash_exec"
        | "run_command"
        | "run_python"
        | "run_python_expr"
        | "wait_terminal"
        | "wait_terminal_ready"
        | "stop_terminal"
        | "run_tests"
        | "smoke_test_custom_tool" => Execute,
        // ── Git ──
        "git_status" | "git_diff" | "git_log" | "git_show" | "git_blame" | "git_grep"
        | "git_branch" | "git_stash_list" | "git_remote" | "git_tag" => GitRead,
        "git_add" | "git_commit_staged" | "git_checkout_files" | "git_create_branch"
        | "git_merge" | "git_push" | "git_stash" | "git_stash_pop" | "git_unstage" => GitWrite,
        // ── Outlook over Microsoft Graph (agenthicc's `tool_network_*`) ──
        "list_emails" | "read_email" | "send_email" | "reply_email" | "search_emails"
        | "move_email" | "list_folders" | "calendar_events" | "create_event" => Network,
        // ── Memory ──
        // `summarize_memory` only lists memories (classified `read` above).
        "remember" | "forget" => Memory,
        // ── Workflow / orchestration control ──
        "handoff_to_agent"
        | "request_fix"
        | "ask_user"
        | "request_plan_approval"
        | "finalize_plan"
        | "exit_workflow"
        | "mark_execute_complete"
        | "approve_review"
        | "reject_review"
        | "set_goals"
        | "append_goal"
        | "insert_goal"
        | "complete_goal"
        | "skip_goal"
        | "set_state"
        | "record_artifact"
        | "request_implementation_approval"
        // goal_list phase controls and goal_flow's transitions.
        | "postpone_phase"
        | "skip_phase"
        | "bring_forward_phase"
        | "delete_phase"
        | "complete_clarification"
        | "finalize_goals"
        | "goal_implemented"
        | "verify_goal"
        | "complete_workflow"
        // parallel_workers (parallel_code_plan).
        | "add_parallel_task"
        | "finalize_task_graph"
        | "dispatch_parallel_workers"
        | "collect_parallel_workers"
        | "integrate_parallel_worker"
        | "mark_parallel_complete"
        // make_book's gates.
        | "submit_toc"
        | "submit_research"
        | "confirm_assets_ready"
        | "confirm_chapter_complete"
        | "confirm_layout_ready"
        | "confirm_front_matter_ready"
        | "confirm_back_matter_ready"
        | "mark_book_complete"
        | "reject_book"
        // make_xaft_tool's transitions and draft path.
        | "submit_tool_plan"
        | "confirm_generation_complete"
        | "approve_tool"
        | "reject_tool"
        | "custom_tool_draft_path"
        | "validate_custom_tool_draft" => Control,
        // ── Subagents (agenthicc's `subagents/`) ──
        // The nine communication tools are agenthicc's `tool_control`: they
        // route messages inside one pool and touch nothing else.
        "answer_subagent"
        | "send_subagent_message"
        | "collect_subagent_results"
        | "send_parent_message"
        | "ask_parent"
        | "poll_agent_messages"
        | "send_peer_message"
        | "ask_peer"
        | "answer_peer" => Control,
        // agenthicc declares no capability on `spawn_subagents`, so its
        // capability gate treats it as undeclared: blocked in Plan, approved
        // per call in Safe. Its workers' own tools are gated on top of that.
        "spawn_subagents" => Undeclared,
        name if name.starts_with("mcp__")
            || name.starts_with("playwright_")
            || name.starts_with("cloakbrowser_")
            || name.starts_with("web_") =>
        {
            Network
        }
        _ => Undeclared,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool in the checked-in coder registry snapshot must be classified;
    /// a new tool that lands as `Undeclared` would silently disappear from
    /// every capability-restricted workflow phase.
    #[test]
    fn every_registered_tool_is_classified() {
        let snapshot = include_str!("../../../docs/tool-registry.txt");
        let unclassified: Vec<&str> = snapshot
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter(|name| capability_of(name) == ToolCapability::Undeclared)
            .collect();
        assert!(
            unclassified.is_empty(),
            "classify these tools in capabilities.rs: {unclassified:?}"
        );
    }

    /// `spawn_subagents` is agenthicc's undeclared tool (Plan blocks it,
    /// Safe asks); its communication tools are control.
    #[test]
    fn subagent_tools_carry_agenthiccs_capabilities() {
        assert_eq!(capability_of("spawn_subagents"), ToolCapability::Undeclared);
        assert!(capability_of("spawn_subagents").is_restricted());
        for tool in [
            "answer_subagent",
            "send_subagent_message",
            "collect_subagent_results",
            "send_parent_message",
            "ask_parent",
            "poll_agent_messages",
            "send_peer_message",
            "ask_peer",
            "answer_peer",
        ] {
            assert_eq!(capability_of(tool), ToolCapability::Control, "{tool}");
        }
    }

    /// The workflow tool groups' tools (`ui_catalog`, `ui_ux_pro_max`,
    /// `reconstruct`) carry agenthicc's capability tags.
    #[test]
    fn workflow_group_tools_are_classified() {
        assert_eq!(capability_of("lookup_component"), ToolCapability::Read);
        assert_eq!(capability_of("list_components"), ToolCapability::Read);
        assert_eq!(
            capability_of("record_reconstruct_screenshot"),
            ToolCapability::Write
        );
        assert_eq!(
            capability_of("request_implementation_approval"),
            ToolCapability::Control
        );
    }

    #[test]
    fn a_declared_custom_capability_classifies_its_tool() {
        assert_eq!(
            capability_of("xaft_test_custom_read"),
            ToolCapability::Undeclared
        );
        declare_custom_capability("xaft_test_custom_read", ToolCapability::Read);
        assert_eq!(capability_of("xaft_test_custom_read"), ToolCapability::Read);
        declare_custom_capability("xaft_test_custom_read", ToolCapability::Read);
        assert_eq!(capability_of("xaft_test_custom_read"), ToolCapability::Read);
    }

    #[test]
    fn a_conflicting_declaration_fails_closed_and_builtins_never_change() {
        declare_custom_capability("xaft_test_custom_twice", ToolCapability::Read);
        declare_custom_capability("xaft_test_custom_twice", ToolCapability::Network);
        assert_eq!(
            capability_of("xaft_test_custom_twice"),
            ToolCapability::Undeclared
        );
        declare_custom_capability("read_file", ToolCapability::Execute);
        assert_eq!(capability_of("read_file"), ToolCapability::Read);
    }

    #[test]
    fn names_round_trip() {
        for cap in ToolCapability::ALL {
            assert_eq!(ToolCapability::parse(cap.as_str()), Some(cap));
        }
        assert_eq!(
            ToolCapability::parse(" GIT_READ "),
            Some(ToolCapability::GitRead)
        );
        assert_eq!(ToolCapability::parse("teleport"), None);
    }

    /// agenthicc's `_restricted_capabilities()`, plus xaft's `memory`.
    #[test]
    fn restricted_caps_match_agenthicc() {
        let restricted: Vec<_> = ToolCapability::ALL
            .iter()
            .copied()
            .filter(|c| c.is_restricted())
            .collect();
        assert_eq!(
            restricted,
            [
                ToolCapability::Write,
                ToolCapability::Execute,
                ToolCapability::GitWrite,
                ToolCapability::Network,
                ToolCapability::Memory,
                ToolCapability::Undeclared,
            ]
        );
        assert!(!ToolCapability::Control.is_restricted());
        assert!(
            ToolCapability::ALL
                .iter()
                .all(|c| !(c.is_read_only() && c.is_restricted()))
        );
        assert!(!capability_of("ask_user").is_restricted());
    }

    #[test]
    fn read_caps_match_agenthicc() {
        assert!(READ_CAPS.iter().all(|c| c.is_read_only()));
        assert_eq!(
            ToolCapability::ALL
                .iter()
                .filter(|c| c.is_read_only())
                .count(),
            READ_CAPS.len()
        );
    }
}
