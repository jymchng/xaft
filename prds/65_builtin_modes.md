---
title: "PRD-65: Built-in Modes — Auto, Plan, Ask, Review, Safe, Debug"
status: draft
version: 0.1.0
created: 2026-06-13
depends-on: 64_mode_system_architecture.md
analog: python/agenthicc/prds/prd-48-builtin-modes.md
---

# PRD-65: Built-in Modes — Auto, Plan, Ask, Review, Safe, Debug

## 1. Problem

PRD 64 defines the `AgentMode` type, `ModeRegistry`, and `ModeManager`
infrastructure. It deliberately leaves the concrete built-in mode catalogue as
out-of-scope. The result is that after PRD 64 ships, the mode system is present
but empty — every session starts in a default mode that users have no name for,
and there are no built-in alternatives to cycle through with Shift+Tab.

This PRD specifies the six built-in modes that ship with xaft out of the box:
**Auto**, **Plan**, **Ask**, **Review**, **Safe**, and **Debug**. These are the
Rust analogs of the six modes in agenthicc PRD-48, adapted for xaft's
role-based agent pipeline, Rust `const`-driven tool filters, and
`SignalBus`-emitted telemetry.

---

## 2. Goals

1. Define and ship six built-in `AgentMode` instances in
   `crates/xaft-tui/src/mode/builtins.rs`.
2. Each mode has a canonical name, label, colour, system patch, optional tool
   filter, and optional post-hook.
3. `build_default_mode_registry()` returns a `ModeRegistry` pre-populated with
   all six modes in the correct cycle order.
4. Tool filters are derived from `const` slices so they are zero-cost to
   construct and testable without runtime state.
5. Full unit and integration test coverage (12 unit, 3 integration).

---

## 3. Non-Goals

- Persistent user preference for active mode between sessions (PRD 66).
- Slash command or keyboard shortcut to switch modes (PRD 66).
- Custom/user-defined modes loaded from `.xaft.toml` (PRD 67).
- The TUI status-bar badge for active mode (PRD 66).
- Agent-side enforcement of mode tool filters (the filter is applied at
  `RunRequest` dispatch time by `ModeManager`; the agent sees only the
  filtered tool set).

---

## 4. Mode Catalogue

| # | Name    | Label    | Colour    | Badge      | Tool Filter          | Purpose                                        |
|---|---------|----------|-----------|------------|----------------------|------------------------------------------------|
| 1 | Auto    | Auto     | Green     | `[AUTO]`   | None (all tools)     | Default unrestricted operation                 |
| 2 | Plan    | Plan     | Yellow    | `[PLAN]`   | Read-only allowlist  | Plan-only; no writes, produces step-by-step plan |
| 3 | Ask     | Ask      | Cyan      | `[ASK]`    | None (all tools)     | Prompt-gated confirmation before every write   |
| 4 | Review  | Review   | Blue      | `[REVIEW]` | Read-only allowlist  | Structured code review; never modifies files   |
| 5 | Safe    | Safe     | Magenta   | `[SAFE]`   | Minimal allowlist    | Hard sandbox; smallest possible tool surface   |
| 6 | Debug   | Debug    | Red       | `[DEBUG]`  | None (all tools)     | Verbose trace; post-hook appends debug footer  |

Cycle order (Shift+Tab): Auto → Plan → Ask → Review → Safe → Debug → Auto …

`[AUTO]` badge is hidden in normal Auto operation (per PRD 66 badge visibility
rules); it is shown only when the TUI is in developer/debug mode. All other
badges are always shown when their mode is active.

---

## 5. File Layout

```
crates/xaft-tui/src/mode/
  mod.rs        ← AgentMode, ModeColour, ModeRegistry, ModeManager (PRD 64)
                   re-exports build_default_mode_registry, builtin_modes
  builtins.rs   ← PLAN_ALLOWED_TOOLS, SAFE_ALLOWED_TOOLS, per-mode constructors,
                   builtin_modes(), build_default_mode_registry(), debug_post_hook()

crates/xaft-tui/tests/
  builtin_modes_integration.rs   ← 3 integration tests
```

---

## 6. Rust Type Reference (from PRD 64)

The types below are defined in `crates/xaft-tui/src/mode/mod.rs` by PRD 64.
They are reproduced here for self-containedness.

```rust
use std::sync::Arc;

/// Type alias for a tool-filter predicate.
/// Returns `true` if the named tool is permitted in this mode.
pub type ToolFilterFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Type alias for a pre-turn hook.
/// Receives the current system prompt; may return a modified version.
pub type PreHookFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Type alias for a post-turn hook.
/// Receives the agent's response text; may return an annotated version.
pub type PostHookFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// ANSI/Ratatui colour assigned to a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeColour {
    Green,
    Yellow,
    Cyan,
    Blue,
    Magenta,
    Red,
}

/// A named operating mode for xaft agents.
#[derive(Clone)]
pub struct AgentMode {
    /// Canonical identifier, e.g. `"auto"`.
    pub name: String,
    /// Human-readable label for TUI display, e.g. `"Auto"`.
    pub label: String,
    /// Short description shown in help overlays.
    pub description: String,
    /// Colour used for the TUI badge.
    pub colour: ModeColour,
    /// Text prepended to the agent's base system prompt.
    /// Empty string → no patch applied.
    pub system_patch: String,
    /// Optional tool filter. `None` means all tools are permitted.
    pub tool_filter: Option<ToolFilterFn>,
    /// Optional hook called before each agent turn with the assembled system
    /// prompt. Returns the (possibly modified) system prompt.
    pub pre_hook: Option<PreHookFn>,
    /// Optional hook called after each agent turn with the agent's response
    /// text. Returns the (possibly modified) response.
    pub post_hook: Option<PostHookFn>,
    /// Identifies origin: `"builtin"` for modes from this PRD, `"user"` for
    /// modes loaded from `.xaft.toml`, `"plugin:<name>"` for plugin-provided.
    pub source_id: String,
}
```

---

## 7. Tool Allowlists

Two `const` slices define the read-only surface and the safe-mode surface.
Both are defined at the top of `builtins.rs`.

### 7.1 Plan / Review allowlist

```rust
/// Tools permitted in Plan and Review modes.
/// All read-only: file reading, directory listing, search, and read-only git.
pub const PLAN_ALLOWED_TOOLS: &[&str] = &[
    // Filesystem reads
    "read_file",
    "read_url",
    "file_exists",
    "file_stat",
    "list_dir",
    "tree",
    "glob",
    "grep_in_file",
    "search_code",
    // Read-only git
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_grep",
];
```

### 7.2 Safe mode allowlist

Safe mode is strictly more restrictive than Plan/Review. It omits `read_url`
(network access), `git_blame` and `git_branch` (less-used), and `git_grep`
(substring search over all commits can be expensive). It also omits
`run_skill`.

```rust
/// Tools permitted in Safe mode.
/// Minimal read-only surface: local filesystem reads + core git inspection.
pub const SAFE_ALLOWED_TOOLS: &[&str] = &[
    // Filesystem reads (local only — no read_url)
    "read_file",
    "file_exists",
    "file_stat",
    "list_dir",
    "tree",
    "glob",
    "grep_in_file",
    "search_code",
    // Core read-only git (no blame/branch/grep)
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
];
```

`SAFE_ALLOWED_TOOLS ⊂ PLAN_ALLOWED_TOOLS` is an invariant verified by the
integration test `test_safe_mode_restricts_further_than_plan`.

---

## 8. Mode Specifications

### 8.1 Auto Mode

Auto is the default mode. It applies no restrictions and no system patch. The
agent runs with the full tool set as configured by `ToolRegistryBuilder`.

```rust
pub fn auto_mode() -> AgentMode {
    AgentMode {
        name: "auto".into(),
        label: "Auto".into(),
        description: "Default mode. All tools available, no additional restrictions.".into(),
        colour: ModeColour::Green,
        system_patch: String::new(),
        tool_filter: None,
        pre_hook: None,
        post_hook: None,
        source_id: "builtin".into(),
    }
}
```

`tool_filter: None` means `ModeManager::apply_to_run_request` passes all tool
names through unchanged.

---

### 8.2 Plan Mode

Plan mode is a read-only thinking mode. The agent explores the codebase and
produces a numbered implementation plan but is forbidden from writing,
modifying, or executing anything. The tool filter is enforced by
`ModeManager`; the system patch reinforces it at the prompt level.

**System patch** (prepended to the base system prompt):

```
[MODE: PLAN]
You are in Plan mode. You MUST NOT write files, edit files, create files, delete
files, execute shell commands, commit code, or call any tool that has side-effects.

Your only allowed tool categories are: reading files, listing directories, searching,
and read-only git operations (git_status, git_diff, git_log, git_show, git_blame).

Instead of acting, produce a numbered step-by-step implementation plan:
  • Step number and title
  • What files are affected and why
  • The specific tool you would use (e.g. `edit_file`) when this step is executed
  • Expected outcome of each step

End your response with:
  "Switch to Auto mode (Shift+Tab) to execute this plan."
```

**Constructor:**

```rust
pub fn plan_mode() -> AgentMode {
    let filter: ToolFilterFn = Arc::new(|tool: &str| {
        PLAN_ALLOWED_TOOLS.contains(&tool)
    });

    AgentMode {
        name: "plan".into(),
        label: "Plan".into(),
        description: "Read-only planning mode. Produces a step-by-step plan without executing.".into(),
        colour: ModeColour::Yellow,
        system_patch: "\
[MODE: PLAN]\n\
You are in Plan mode. You MUST NOT write files, edit files, create files, delete\n\
files, execute shell commands, commit code, or call any tool that has side-effects.\n\
\n\
Your only allowed tool categories are: reading files, listing directories, searching,\n\
and read-only git operations (git_status, git_diff, git_log, git_show, git_blame).\n\
\n\
Instead of acting, produce a numbered step-by-step implementation plan:\n\
  \u{2022} Step number and title\n\
  \u{2022} What files are affected and why\n\
  \u{2022} The specific tool you would use (e.g. `edit_file`) when this step is executed\n\
  \u{2022} Expected outcome of each step\n\
\n\
End your response with:\n\
  \"Switch to Auto mode (Shift+Tab) to execute this plan.\"\n\
".into(),
        tool_filter: Some(filter),
        pre_hook: None,
        post_hook: None,
        source_id: "builtin".into(),
    }
}
```

---

### 8.3 Ask Mode

Ask mode leaves all tools available but instructs the agent to explain each
write/exec operation and request explicit user confirmation before calling it.
This is a prompt-level enforcement; xaft's existing hard approval gate
(`bash_exec` always requires `ApprovalGate` confirmation) provides an
additional mechanical layer.

`tool_filter: None` — the agent sees all tools. The system patch is the only
restriction.

**System patch:**

```
[MODE: ASK]
You are in Ask mode. You have access to all tools, but you MUST explain and
confirm every write or execute operation before calling it.

Before calling any of the following tools, output a brief explanation of what
you intend to do and why, then state: "Shall I proceed? (y/n)". Wait for user
confirmation before issuing the tool call:
  write_file, edit_file, create_file, delete_file, move_file, copy_file,
  patch_file, bash_exec, git_add, git_commit_staged, git_checkout, git_stash,
  git_push, git_pull, git_merge, run_skill

Read-only tools (read_file, list_dir, git_status, git_diff, etc.) may be
called freely without confirmation.

Note: bash_exec always triggers xaft's built-in approval gate regardless of
mode — Ask mode extends this requirement to all write tools via this prompt.
```

**Constructor:**

```rust
pub fn ask_mode() -> AgentMode {
    AgentMode {
        name: "ask".into(),
        label: "Ask".into(),
        description: "Confirmation-gated mode. Agent explains and requests approval before every write or exec.".into(),
        colour: ModeColour::Cyan,
        system_patch: "\
[MODE: ASK]\n\
You are in Ask mode. You have access to all tools, but you MUST explain and\n\
confirm every write or execute operation before calling it.\n\
\n\
Before calling any of the following tools, output a brief explanation of what\n\
you intend to do and why, then state: \"Shall I proceed? (y/n)\". Wait for user\n\
confirmation before issuing the tool call:\n\
  write_file, edit_file, create_file, delete_file, move_file, copy_file,\n\
  patch_file, bash_exec, git_add, git_commit_staged, git_checkout, git_stash,\n\
  git_push, git_pull, git_merge, run_skill\n\
\n\
Read-only tools (read_file, list_dir, git_status, git_diff, etc.) may be\n\
called freely without confirmation.\n\
\n\
Note: bash_exec always triggers xaft's built-in approval gate regardless of\n\
mode \u{2014} Ask mode extends this requirement to all write tools via this prompt.\n\
".into(),
        tool_filter: None,
        pre_hook: None,
        post_hook: None,
        source_id: "builtin".into(),
    }
}
```

---

### 8.4 Review Mode

Review mode is a structured code-review mode. Like Plan mode, it uses the
read-only tool allowlist. The system patch directs the agent to produce a
four-section structured review rather than plan steps.

**System patch:**

```
[MODE: REVIEW]
You are in Review mode. Your role is to review code, files, diffs, and changes.
You MUST NOT write, modify, delete, or execute anything.

Produce a structured review using exactly these sections:

  1. Summary — what does this code/diff do? One to three sentences.
  2. Issues — bugs, security concerns, performance problems, code style
     violations. Number each issue; include file and line reference where
     possible.
  3. Suggestions — specific improvements with file/line references.
     If none, write "No suggestions."
  4. Assessment — one of:
       Approve            (ready to merge as-is)
       Request Changes    (blocking issues found)
       Needs Discussion   (questions or trade-offs to resolve first)

Use only read-only tools. Do not create, modify, or delete any file.
```

**Constructor:**

```rust
pub fn review_mode() -> AgentMode {
    let filter: ToolFilterFn = Arc::new(|tool: &str| {
        PLAN_ALLOWED_TOOLS.contains(&tool)
    });

    AgentMode {
        name: "review".into(),
        label: "Review".into(),
        description: "Structured code-review mode. Read-only; produces a four-section review.".into(),
        colour: ModeColour::Blue,
        system_patch: "\
[MODE: REVIEW]\n\
You are in Review mode. Your role is to review code, files, diffs, and changes.\n\
You MUST NOT write, modify, delete, or execute anything.\n\
\n\
Produce a structured review using exactly these sections:\n\
\n\
  1. Summary \u{2014} what does this code/diff do? One to three sentences.\n\
  2. Issues \u{2014} bugs, security concerns, performance problems, code style\n\
     violations. Number each issue; include file and line reference where\n\
     possible.\n\
  3. Suggestions \u{2014} specific improvements with file/line references.\n\
     If none, write \"No suggestions.\"\n\
  4. Assessment \u{2014} one of:\n\
       Approve            (ready to merge as-is)\n\
       Request Changes    (blocking issues found)\n\
       Needs Discussion   (questions or trade-offs to resolve first)\n\
\n\
Use only read-only tools. Do not create, modify, or delete any file.\n\
".into(),
        tool_filter: Some(filter),
        pre_hook: None,
        post_hook: None,
        source_id: "builtin".into(),
    }
}
```

---

### 8.5 Safe Mode

Safe mode is the strictest sandbox. It uses the minimal `SAFE_ALLOWED_TOOLS`
allowlist and informs the agent that it is operating in a restricted
environment. The system patch explains the restrictions and directs the agent
to suggest switching modes rather than failing silently.

**System patch:**

```
[MODE: SAFE]
You are in Safe mode. This is the most restricted operating mode.

You may ONLY use the following tools:
  read_file, file_exists, file_stat, list_dir, tree, glob, grep_in_file,
  search_code, git_status, git_diff, git_log, git_show

All write operations, shell execution, git mutation, network access, and
skill execution are disabled.

If the user's task requires write access, explain that Safe mode prevents it
and ask them to switch to a different mode: press Shift+Tab to cycle modes.
Do not attempt to call tools outside the list above.
```

**Constructor:**

```rust
pub fn safe_mode() -> AgentMode {
    let filter: ToolFilterFn = Arc::new(|tool: &str| {
        SAFE_ALLOWED_TOOLS.contains(&tool)
    });

    AgentMode {
        name: "safe".into(),
        label: "Safe".into(),
        description: "Strictest sandbox. Minimal read-only tool set; no network, no writes, no shell.".into(),
        colour: ModeColour::Magenta,
        system_patch: "\
[MODE: SAFE]\n\
You are in Safe mode. This is the most restricted operating mode.\n\
\n\
You may ONLY use the following tools:\n\
  read_file, file_exists, file_stat, list_dir, tree, glob, grep_in_file,\n\
  search_code, git_status, git_diff, git_log, git_show\n\
\n\
All write operations, shell execution, git mutation, network access, and\n\
skill execution are disabled.\n\
\n\
If the user's task requires write access, explain that Safe mode prevents it\n\
and ask them to switch to a different mode: press Shift+Tab to cycle modes.\n\
Do not attempt to call tools outside the list above.\n\
".into(),
        tool_filter: Some(filter),
        pre_hook: None,
        post_hook: None,
        source_id: "builtin".into(),
    }
}
```

---

### 8.6 Debug Mode

Debug mode is unrestricted (no tool filter) but adds verbose tracing to every
agent response. The system patch instructs the agent to append a
`--- DEBUG ---` section. The `post_hook` appends a static footer marker to the
transcript so the debug annotation is always visible even if the agent omits
the section (e.g. in streaming contexts where the agent's own section was
truncated).

Live token counts and cost stats are already shown in the xaft TUI status line
via `XaftLlmCallStarting` (with `call_index`) and `XaftAgentOutput` (with
token metadata from the agtrs `ModelCallComplete` signal). The hook does not
attempt to aggregate these synchronously; it only marks the transcript entry.

**System patch:**

```
[MODE: DEBUG]
You are in Debug mode. After every response, append a section with this
exact heading:

--- DEBUG ---

Include in that section:
  • Tools called this turn, in order (tool name, brief rationale)
  • Any retries or fallbacks (tool failed, retried with different args, etc.)
  • Your decision rationale: why did you choose this approach over alternatives?

Keep the DEBUG section concise. One line per tool call is sufficient.
--- END DEBUG FORMAT ---
```

**Post-hook:**

```rust
/// Returns a `PostHookFn` that appends a static debug footer to the agent's
/// response transcript entry.
///
/// Live per-turn token/cost stats are displayed in the TUI status line via
/// `XaftLlmCallStarting` and `ModelCallComplete` signal subscriptions; this
/// hook just marks the transcript so the debug annotation is anchored.
pub fn debug_post_hook() -> PostHookFn {
    Arc::new(|response: &str| -> String {
        format!(
            "{response}\n\n```\n[xaft:debug] response appended \u{2014} check TUI for live token/cost stats\n```"
        )
    })
}
```

**Constructor:**

```rust
pub fn debug_mode() -> AgentMode {
    AgentMode {
        name: "debug".into(),
        label: "Debug".into(),
        description: "Verbose debug mode. All tools available; agent appends per-turn trace section.".into(),
        colour: ModeColour::Red,
        system_patch: "\
[MODE: DEBUG]\n\
You are in Debug mode. After every response, append a section with this\n\
exact heading:\n\
\n\
--- DEBUG ---\n\
\n\
Include in that section:\n\
  \u{2022} Tools called this turn, in order (tool name, brief rationale)\n\
  \u{2022} Any retries or fallbacks (tool failed, retried with different args, etc.)\n\
  \u{2022} Your decision rationale: why did you choose this approach over alternatives?\n\
\n\
Keep the DEBUG section concise. One line per tool call is sufficient.\n\
--- END DEBUG FORMAT ---\n\
".into(),
        tool_filter: None,
        pre_hook: None,
        post_hook: Some(debug_post_hook()),
        source_id: "builtin".into(),
    }
}
```

---

## 9. `builtin_modes()` and `build_default_mode_registry()`

Both functions are defined in `crates/xaft-tui/src/mode/builtins.rs`.

```rust
/// Returns the six built-in modes in cycle order.
///
/// The order is: Auto → Plan → Ask → Review → Safe → Debug.
/// This matches the Shift+Tab cycle order in the TUI (PRD 66).
pub fn builtin_modes() -> Vec<AgentMode> {
    vec![
        auto_mode(),
        plan_mode(),
        ask_mode(),
        review_mode(),
        safe_mode(),
        debug_mode(),
    ]
}

/// Constructs a `ModeRegistry` pre-populated with all six built-in modes.
///
/// Call this at TUI startup to seed the registry before any user-defined
/// modes are loaded from `.xaft.toml`.
///
/// ```rust
/// use xaft_tui::mode::builtins::build_default_mode_registry;
///
/// let registry = build_default_mode_registry();
/// assert_eq!(registry.len(), 6);
/// assert_eq!(registry.get("auto").unwrap().colour, ModeColour::Green);
/// ```
pub fn build_default_mode_registry() -> ModeRegistry {
    let mut reg = ModeRegistry::new();
    for mode in builtin_modes() {
        reg.register(mode);
    }
    reg
}
```

**Re-exports in `crates/xaft-tui/src/mode/mod.rs`:**

```rust
pub mod builtins;

pub use builtins::{build_default_mode_registry, builtin_modes};
```

**Re-exports in `crates/xaft-tui/src/lib.rs`:**

```rust
pub use mode::{
    AgentMode, ModeColour, ModeManager, ModeRegistry,
    build_default_mode_registry, builtin_modes,
};
```

---

## 10. Unit Tests

All unit tests live in `crates/xaft-tui/src/mode/builtins.rs` inside a
`#[cfg(test)] mod tests` block.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. Auto: tool_filter is None ──────────────────────────────────────
    #[test]
    fn test_auto_no_filter() {
        let mode = auto_mode();
        assert!(mode.tool_filter.is_none(), "Auto mode must have no tool filter");
    }

    // ── 2. Plan: write_file is blocked ────────────────────────────────────
    #[test]
    fn test_plan_blocks_write_file() {
        let mode = plan_mode();
        let filter = mode.tool_filter.expect("Plan mode must have a tool filter");
        assert!(
            !filter("write_file"),
            "Plan mode must block write_file"
        );
    }

    // ── 3. Plan: read_file is allowed ─────────────────────────────────────
    #[test]
    fn test_plan_allows_read_file() {
        let mode = plan_mode();
        let filter = mode.tool_filter.expect("Plan mode must have a tool filter");
        assert!(
            filter("read_file"),
            "Plan mode must allow read_file"
        );
    }

    // ── 4. Plan: system_patch contains "MUST NOT" ─────────────────────────
    #[test]
    fn test_plan_patches_system_prompt() {
        let mode = plan_mode();
        assert!(
            mode.system_patch.contains("MUST NOT"),
            "Plan system_patch must contain 'MUST NOT'"
        );
    }

    // ── 5. Review: bash_exec is blocked ───────────────────────────────────
    #[test]
    fn test_review_blocks_bash_exec() {
        let mode = review_mode();
        let filter = mode.tool_filter.expect("Review mode must have a tool filter");
        assert!(
            !filter("bash_exec"),
            "Review mode must block bash_exec"
        );
    }

    // ── 6. Safe: git_commit_staged, edit_file, run_skill all blocked ──────
    #[test]
    fn test_safe_most_restrictive() {
        let mode = safe_mode();
        let filter = mode.tool_filter.expect("Safe mode must have a tool filter");
        assert!(!filter("git_commit_staged"), "Safe must block git_commit_staged");
        assert!(!filter("edit_file"),         "Safe must block edit_file");
        assert!(!filter("run_skill"),         "Safe must block run_skill");
    }

    // ── 7. Safe: git_log is allowed ───────────────────────────────────────
    #[test]
    fn test_safe_allows_git_log() {
        let mode = safe_mode();
        let filter = mode.tool_filter.expect("Safe mode must have a tool filter");
        assert!(
            filter("git_log"),
            "Safe mode must allow git_log"
        );
    }

    // ── 8. Debug: post_hook is Some ───────────────────────────────────────
    #[test]
    fn test_debug_has_post_hook() {
        let mode = debug_mode();
        assert!(
            mode.post_hook.is_some(),
            "Debug mode must have a post_hook"
        );
    }

    // ── 9. Debug post-hook appends footer ────────────────────────────────
    #[test]
    fn test_debug_post_hook_appends_footer() {
        let hook = debug_post_hook();
        let result = hook("response text");
        assert!(
            result.contains("response text"),
            "post_hook output must contain original response"
        );
        assert!(
            result.contains("[xaft:debug]"),
            "post_hook output must contain '[xaft:debug]' footer"
        );
    }

    // ── 10. builtin_modes() cycle order ──────────────────────────────────
    #[test]
    fn test_cycle_order_matches_catalogue() {
        let registry = build_default_mode_registry();
        let names: Vec<&str> = registry
            .all()
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["auto", "plan", "ask", "review", "safe", "debug"],
            "built-in modes must be registered in cycle order"
        );
    }

    // ── 11. All built-ins have source_id == "builtin" ────────────────────
    #[test]
    fn test_all_modes_have_source_builtin() {
        for mode in builtin_modes() {
            assert_eq!(
                mode.source_id, "builtin",
                "mode '{}' must have source_id == \"builtin\"",
                mode.name
            );
        }
    }

    // ── 12. Ask: tool_filter is None (prompt-only gating) ────────────────
    #[test]
    fn test_ask_has_no_filter() {
        let mode = ask_mode();
        assert!(
            mode.tool_filter.is_none(),
            "Ask mode must have no tool filter (confirmation is prompt-enforced)"
        );
    }
}
```

---

## 11. Integration Tests

Integration tests live in
`crates/xaft-tui/tests/builtin_modes_integration.rs`.

```rust
//! Integration tests for the built-in mode catalogue (PRD-65).

use xaft_tui::mode::builtins::{
    PLAN_ALLOWED_TOOLS, SAFE_ALLOWED_TOOLS, build_default_mode_registry,
};

// ── 1. Registry contains exactly six modes ────────────────────────────────
#[test]
fn test_registry_has_all_six_modes() {
    let registry = build_default_mode_registry();
    assert_eq!(
        registry.len(),
        6,
        "Default mode registry must contain exactly 6 built-in modes"
    );
}

// ── 2. Plan mode ModeManager removes write tools from RunRequest ──────────
//
// Uses a dummy RunRequest with an injected tool list to verify that
// ModeManager::apply_to_run_request filters out write tools when the
// active mode is Plan.
#[test]
fn test_plan_mode_manager_apply_restricts_tools() {
    use xaft_tui::mode::{ModeManager, ModeRegistry};
    use xaft_tui::mode::builtins::plan_mode;

    let mut manager = ModeManager::new(ModeRegistry::new());
    manager.register_and_activate(plan_mode());

    // Tool names that a full-capability run would include.
    let all_tools = vec![
        "read_file", "list_dir", "git_status",   // should survive
        "write_file", "edit_file", "bash_exec",   // should be removed
        "git_commit_staged", "delete_file",        // should be removed
    ];

    let filtered = manager.filter_tools(&all_tools);

    // Read-only tools survive.
    assert!(filtered.contains(&"read_file"),  "read_file must pass Plan filter");
    assert!(filtered.contains(&"list_dir"),   "list_dir must pass Plan filter");
    assert!(filtered.contains(&"git_status"), "git_status must pass Plan filter");

    // Write tools are removed.
    assert!(!filtered.contains(&"write_file"),        "write_file must be blocked by Plan");
    assert!(!filtered.contains(&"edit_file"),         "edit_file must be blocked by Plan");
    assert!(!filtered.contains(&"bash_exec"),         "bash_exec must be blocked by Plan");
    assert!(!filtered.contains(&"git_commit_staged"), "git_commit_staged must be blocked by Plan");
    assert!(!filtered.contains(&"delete_file"),       "delete_file must be blocked by Plan");
}

// ── 3. SAFE_ALLOWED_TOOLS ⊂ PLAN_ALLOWED_TOOLS ──────────────────────────
#[test]
fn test_safe_mode_restricts_further_than_plan() {
    for tool in SAFE_ALLOWED_TOOLS {
        assert!(
            PLAN_ALLOWED_TOOLS.contains(tool),
            "tool '{}' is in SAFE_ALLOWED_TOOLS but not in PLAN_ALLOWED_TOOLS — \
             Safe must be a strict subset of Plan",
            tool
        );
    }
    // Also verify Safe is strictly smaller (not equal).
    assert!(
        SAFE_ALLOWED_TOOLS.len() < PLAN_ALLOWED_TOOLS.len(),
        "SAFE_ALLOWED_TOOLS must be strictly smaller than PLAN_ALLOWED_TOOLS"
    );
}
```

---

## 12. `ModeManager::filter_tools` Contract (integration-test dependency)

The integration test `test_plan_mode_manager_apply_restricts_tools` calls
`ModeManager::filter_tools`. This method must be provided by PRD 64's
`ModeManager` implementation. Its signature:

```rust
impl ModeManager {
    /// Filters a slice of tool name strings using the active mode's
    /// `tool_filter`. Returns only the names that pass.
    ///
    /// If the active mode has `tool_filter: None`, all names are returned
    /// unchanged (Auto / Ask / Debug behaviour).
    pub fn filter_tools<'a>(&self, tools: &[&'a str]) -> Vec<&'a str> {
        match &self.active_mode().tool_filter {
            None => tools.to_vec(),
            Some(f) => tools.iter().copied().filter(|t| f(t)).collect(),
        }
    }
}
```

If PRD 64 did not include `filter_tools`, add it as part of this PRD's
implementation.

---

## 13. Future Built-in Modes (Planned, Not in v1)

These modes are analogous to agenthicc's extended catalogue. They are named
here to reserve their canonical `name` strings and prevent collisions with
user-defined modes.

| Name      | Label     | Colour  | Notes                                                                    |
|-----------|-----------|---------|--------------------------------------------------------------------------|
| `pair`    | Pair      | White   | Narrates every decision out loud; "rubber-duck debugging" companion mode |
| `teach`   | Teach     | Green   | Explains rationale rather than just acting; pedagogical mode             |
| `strict`  | Strict    | Yellow  | Enforces AGENTS.md / CLAUDE.md rules exactly; refuses out-of-scope tasks |
| `headless`| Headless  | Grey    | Structured JSON output to stdout; designed for CI pipelines              |
| `yolo`    | Yolo      | Red     | Skips all approval gates; `dangerously_skip_permissions = true` by mode  |

`yolo` in particular must NOT be implemented until the approval gate bypass is
gated behind an explicit runtime flag (already present as
`RunRequest::dangerously_skip_permissions`) and a TUI danger confirmation
modal. It is listed here only to reserve the name.

---

## 14. Implementation Checklist

- [ ] Create `crates/xaft-tui/src/mode/builtins.rs`
  - [ ] `PLAN_ALLOWED_TOOLS` const slice
  - [ ] `SAFE_ALLOWED_TOOLS` const slice
  - [ ] `auto_mode()` constructor
  - [ ] `plan_mode()` constructor
  - [ ] `ask_mode()` constructor
  - [ ] `review_mode()` constructor
  - [ ] `safe_mode()` constructor
  - [ ] `debug_post_hook()` function
  - [ ] `debug_mode()` constructor
  - [ ] `builtin_modes()` function
  - [ ] `build_default_mode_registry()` function
  - [ ] `#[cfg(test)] mod tests` with all 12 unit tests
- [ ] Update `crates/xaft-tui/src/mode/mod.rs`
  - [ ] `pub mod builtins;`
  - [ ] `pub use builtins::{build_default_mode_registry, builtin_modes};`
- [ ] Update `crates/xaft-tui/src/lib.rs`
  - [ ] Re-export `build_default_mode_registry` and `builtin_modes`
- [ ] Add `ModeManager::filter_tools` if not present from PRD 64
- [ ] Create `crates/xaft-tui/tests/builtin_modes_integration.rs`
  - [ ] `test_registry_has_all_six_modes`
  - [ ] `test_plan_mode_manager_apply_restricts_tools`
  - [ ] `test_safe_mode_restricts_further_than_plan`
- [ ] `cargo test -p xaft-tui` — all tests pass
- [ ] `cargo clippy -p xaft-tui` — no warnings
- [ ] `cargo fmt -p xaft-tui -- --check` — clean
