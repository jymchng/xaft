---
title: "PRD-64: Mode System Architecture — Agent Operational Modes"
status: draft
version: 0.1.0
created: 2026-06-13
analog: python/agenthicc/prds/prd-47-mode-system-architecture.md
---

# PRD-64: Mode System Architecture — Agent Operational Modes

## Overview

This PRD specifies the Mode System for xaft: a structured mechanism for switching the agent's operating behaviour at the TUI level. A "mode" is a named operational profile that (a) prepends a system-prompt patch to every agent run, (b) optionally restricts which tools are available, and (c) runs optional pre/post hooks that can transform user text before submission and agent responses after receipt.

The user cycles modes with **Shift+Tab** in the TUI or by typing `/mode <name>`. The active mode badge is shown in the prompt line (e.g., `[AUTO]` in green, `[PLAN]` in cyan). Modes are designed to be extended by skills and plugins via `source_id`.

The analogue in the Python codebase is PRD-47 (`ModeManager`, `ModeRegistry`, `Mode` dataclass).

---

## 1. Core types — `AgentMode` struct

### 1.1 File location

```
crates/xaft-tui/src/mode/mod.rs
```

### 1.2 Type aliases

```rust
use std::sync::Arc;

/// Returns `true` if a tool with the given name should be available in this mode.
///
/// The filter receives only the snake_case tool name (e.g. `"bash_exec"`,
/// `"write_file"`). It does NOT receive an execution context or input
/// arguments — it is called once per tool at run-request construction time,
/// not at call time.
pub type ToolFilterFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Called immediately before the user's text is submitted to the runtime.
/// Returns the (possibly modified) text that the agent will receive.
///
/// Signature: `(user_text: &str) -> String`
pub type PreHookFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Called after the agent's text response is received.
/// Returns the (possibly modified) response shown in the transcript.
///
/// Signature: `(agent_response: &str) -> String`
pub type PostHookFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
```

**Why `Arc<dyn Fn>` and not bare `fn` pointers?**

`fn` pointers can only represent functions that capture no state. Many real-world mode implementations need to capture state. For example, a Debug mode that counts tokens-per-turn needs an `Arc<AtomicU64>` counter; a Verbose mode that appends context from the current session needs an `Arc<AppState>` reference; a Safe mode built by a plugin may need to read a config value from a `Arc<XaftConfig>`. `Arc<dyn Fn(...) -> ...>` handles all of these uniformly. The `Arc` wrapper makes the function cheaply cloneable into `RunRequest` and across async boundaries without copying the closure or the captured state.

### 1.3 `ModeColour`

```rust
/// Terminal colour used for the mode badge in the prompt line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeColour {
    Green,
    Yellow,
    Cyan,
    Blue,
    Magenta,
    Red,
    White,
}

impl ModeColour {
    /// Returns the ANSI escape colour name understood by `crossterm::style::Color`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Green   => "green",
            Self::Yellow  => "yellow",
            Self::Cyan    => "cyan",
            Self::Blue    => "blue",
            Self::Magenta => "magenta",
            Self::Red     => "red",
            Self::White   => "white",
        }
    }
}
```

### 1.4 `AgentMode` struct

```rust
/// A named operational mode that patches the agent's system prompt, tool
/// availability, and user/response transformation.
pub struct AgentMode {
    /// Internal identifier, used as the key in `ModeRegistry`.
    /// Examples: `"auto"`, `"plan"`, `"safe"`.
    pub name: String,

    /// Short uppercase label shown in the TUI prompt badge.
    /// Examples: `"AUTO"`, `"PLAN"`, `"SAFE"`.
    pub label: String,

    /// Human-readable description shown in `/mode` help output.
    pub description: String,

    /// Colour of the badge in the prompt line.
    pub colour: ModeColour,

    /// Text prepended to the base system prompt when non-empty.
    ///
    /// This is inserted before the role-specific prompt so it takes
    /// precedence. Typically one or two sentences of constraint or
    /// instruction.
    ///
    /// Example (Plan mode):
    /// "You are in PLAN-ONLY mode. Do NOT call write_file, edit_file,
    /// create_file, delete_file, or any destructive tool. Only plan and
    /// read."
    pub system_patch: String,

    /// Optional filter applied to the tool set at `RunRequest` construction
    /// time. `None` means "allow all tools". `Some(f)` means "only include
    /// tools for which `f(tool_name)` returns `true`".
    pub tool_filter: Option<ToolFilterFn>,

    /// Optional pre-hook. When `Some`, called with the user's raw text before
    /// it is sent to the runtime. The returned string replaces the text.
    pub pre_hook: Option<PreHookFn>,

    /// Optional post-hook. When `Some`, called with the agent's final response
    /// text before it is committed to the transcript. The returned string
    /// replaces the response.
    pub post_hook: Option<PostHookFn>,

    /// Provenance identifier for plugin/skill lifecycle management.
    ///
    /// Built-in modes use `"builtin"`. Skill-contributed modes use
    /// `"skill:<slug>"`. User-defined modes use `"user"`.
    ///
    /// `ModeRegistry::unregister_source` removes all modes with a given
    /// `source_id`, which is how skill unload cleans up its modes.
    pub source_id: String,
}
```

`AgentMode` does **not** derive `Clone` because `Arc<dyn Fn>` is cheaply clone-able but semantically the mode is a singleton; callers hold `&AgentMode` references from the registry rather than cloning. `Debug` is implemented manually:

```rust
impl std::fmt::Debug for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMode")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("colour", &self.colour)
            .field("source_id", &self.source_id)
            .field("has_tool_filter", &self.tool_filter.is_some())
            .field("has_pre_hook", &self.pre_hook.is_some())
            .field("has_post_hook", &self.post_hook.is_some())
            .finish()
    }
}
```

### 1.5 `AgentModeBuilder`

Constructing an `AgentMode` struct literal is verbose. The builder provides a fluent API:

```rust
/// Fluent builder for `AgentMode`.
pub struct AgentModeBuilder {
    name: String,
    label: String,
    description: String,
    colour: ModeColour,
    system_patch: String,
    tool_filter: Option<ToolFilterFn>,
    pre_hook: Option<PreHookFn>,
    post_hook: Option<PostHookFn>,
    source_id: String,
}

impl AgentModeBuilder {
    /// Start a new builder. `name` is the internal key; `label` is the
    /// short badge string.
    pub fn new(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            description: String::new(),
            colour: ModeColour::White,
            system_patch: String::new(),
            tool_filter: None,
            pre_hook: None,
            post_hook: None,
            source_id: "builtin".to_string(),
        }
    }

    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }

    pub fn colour(mut self, c: ModeColour) -> Self {
        self.colour = c;
        self
    }

    pub fn system_patch(mut self, patch: impl Into<String>) -> Self {
        self.system_patch = patch.into();
        self
    }

    pub fn tool_filter(mut self, f: ToolFilterFn) -> Self {
        self.tool_filter = Some(f);
        self
    }

    pub fn pre_hook(mut self, f: PreHookFn) -> Self {
        self.pre_hook = Some(f);
        self
    }

    pub fn post_hook(mut self, f: PostHookFn) -> Self {
        self.post_hook = Some(f);
        self
    }

    pub fn source_id(mut self, id: impl Into<String>) -> Self {
        self.source_id = id.into();
        self
    }

    pub fn build(self) -> AgentMode {
        AgentMode {
            name: self.name,
            label: self.label,
            description: self.description,
            colour: self.colour,
            system_patch: self.system_patch,
            tool_filter: self.tool_filter,
            pre_hook: self.pre_hook,
            post_hook: self.post_hook,
            source_id: self.source_id,
        }
    }
}
```

### 1.6 Built-in modes

The three built-in modes must be constructed by `ModeRegistry::default()`:

```rust
fn builtin_auto() -> AgentMode {
    AgentModeBuilder::new("auto", "AUTO")
        .description("Default mode. All tools available, no system prompt patch.")
        .colour(ModeColour::Green)
        .source_id("builtin")
        .build()
}

fn builtin_plan() -> AgentMode {
    AgentModeBuilder::new("plan", "PLAN")
        .description(
            "Plan-only mode. Read and list tools only; no file writes or shell execution."
        )
        .colour(ModeColour::Cyan)
        .system_patch(
            "You are in PLAN-ONLY mode. Do NOT call write_file, edit_file, create_file, \
             delete_file, move_file, copy_file, patch_file, bash_exec, git_add, \
             git_commit_staged, git_checkout, git_stash, git_push, or git_pull. \
             Only analyse, read, and plan.",
        )
        .tool_filter(Arc::new(|name: &str| {
            // Whitelist: only read + git-read tools.
            matches!(
                name,
                "read_file"
                    | "list_dir"
                    | "tree"
                    | "file_stat"
                    | "glob"
                    | "grep_in_file"
                    | "file_exists"
                    | "read_url"
                    | "git_status"
                    | "git_diff"
                    | "git_log"
                    | "git_show"
                    | "git_blame"
                    | "git_branch"
                    | "git_grep"
            )
        }))
        .source_id("builtin")
        .build()
}

fn builtin_safe() -> AgentMode {
    AgentModeBuilder::new("safe", "SAFE")
        .description(
            "Safe mode. All read tools available; destructive writes and shell \
             execution require explicit approval (approval gate is not bypassed \
             even if auto_approve is set)."
        )
        .colour(ModeColour::Yellow)
        .system_patch(
            "You are in SAFE mode. Prefer read operations. Before writing any file, \
             explain your reasoning. Never run shell commands unless explicitly asked.",
        )
        // Safe mode does not filter tools (tools still appear in the registry)
        // but disables auto-approve for write tools — enforced by the runtime
        // via RunRequest.dangerously_skip_permissions = false.
        .source_id("builtin")
        .build()
}
```

---

## 2. `ModeRegistry`

### 2.1 File location

```
crates/xaft-tui/src/mode/registry.rs
```

### 2.2 Data structure

```rust
use std::collections::HashMap;
use super::{AgentMode, AgentModeBuilder, ModeColour};

/// Ordered registry of `AgentMode` entries.
///
/// Modes are stored in insertion order (the `Vec` drives `next_after`
/// cycling). The `HashMap<String, usize>` index gives O(1) name lookup.
///
/// Invariant: every `name` in `modes` has a matching key in `index` and
/// vice versa. `register()` and `unregister_source()` are the only
/// mutation points and both maintain the invariant.
pub struct ModeRegistry {
    /// Ordered list of modes (defines cycling order).
    modes: Vec<AgentMode>,
    /// `name → index into modes`.
    index: HashMap<String, usize>,
}
```

### 2.3 `impl ModeRegistry`

```rust
impl ModeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            modes: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Register `mode`. If a mode with the same `name` already exists it is
    /// replaced in-place (preserving its position in the cycling order).
    /// If the name is new it is appended.
    pub fn register(&mut self, mode: AgentMode) {
        if let Some(&idx) = self.index.get(&mode.name) {
            // Replace in-place — preserve cycling position.
            self.modes[idx] = mode;
        } else {
            let idx = self.modes.len();
            self.index.insert(mode.name.clone(), idx);
            self.modes.push(mode);
        }
    }

    /// Remove all modes whose `source_id` matches `source_id`.
    ///
    /// Builtins use `source_id = "builtin"`. Plugin unload calls this with
    /// `"skill:<slug>"` to clean up contributed modes.
    ///
    /// After removal the index is rebuilt from scratch to keep positions
    /// consistent.
    pub fn unregister_source(&mut self, source_id: &str) {
        self.modes.retain(|m| m.source_id != source_id);
        self.rebuild_index();
    }

    /// Look up a mode by name.
    pub fn get(&self, name: &str) -> Option<&AgentMode> {
        self.index.get(name).map(|&idx| &self.modes[idx])
    }

    /// Return the mode that follows `current_name` in cycling order,
    /// wrapping around to the first mode.
    ///
    /// Panics if the registry is empty (caller must ensure at least one
    /// mode exists before calling).
    pub fn next_after(&self, current_name: &str) -> &AgentMode {
        assert!(!self.modes.is_empty(), "ModeRegistry::next_after called on empty registry");
        let current_idx = self.index.get(current_name).copied().unwrap_or(0);
        let next_idx = (current_idx + 1) % self.modes.len();
        &self.modes[next_idx]
    }

    /// All modes in cycling order.
    pub fn all_modes(&self) -> &[AgentMode] {
        &self.modes
    }

    pub fn len(&self) -> usize {
        self.modes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn rebuild_index(&mut self) {
        self.index.clear();
        for (i, mode) in self.modes.iter().enumerate() {
            self.index.insert(mode.name.clone(), i);
        }
    }
}

impl Default for ModeRegistry {
    /// Construct a registry pre-populated with the three built-in modes:
    /// Auto → Plan → Safe (cycling order).
    fn default() -> Self {
        let mut r = Self::new();
        r.register(super::builtin_auto());
        r.register(super::builtin_plan());
        r.register(super::builtin_safe());
        r
    }
}
```

---

## 3. `ModeManager`

### 3.1 File location

```
crates/xaft-tui/src/mode/manager.rs
```

### 3.2 `ModeError`

Library-crate error type (never `anyhow`):

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModeError {
    #[error("unknown mode: '{0}'")]
    UnknownMode(String),

    #[error("mode registry is empty")]
    EmptyRegistry,
}
```

### 3.3 `ModeManager` struct

```rust
use super::registry::ModeRegistry;
use super::{AgentMode, ToolFilterFn};
use xaft_runtime::dispatch::RunRequest;

pub struct ModeManager {
    registry: ModeRegistry,
    /// Index into `registry.all_modes()` for the currently active mode.
    active_idx: usize,
}
```

### 3.4 `impl ModeManager`

```rust
impl ModeManager {
    /// Create a `ModeManager` wrapping `registry`.
    /// The first mode in the registry is active by default.
    pub fn new(registry: ModeRegistry) -> Result<Self, ModeError> {
        if registry.is_empty() {
            return Err(ModeError::EmptyRegistry);
        }
        Ok(Self { registry, active_idx: 0 })
    }

    /// Create a `ModeManager` with the default built-in registry.
    /// Active mode is "auto".
    pub fn default_builtin() -> Self {
        Self {
            registry: ModeRegistry::default(),
            active_idx: 0,
        }
    }

    /// Return a reference to the currently active mode.
    pub fn active(&self) -> &AgentMode {
        &self.registry.all_modes()[self.active_idx]
    }

    /// Advance to the next mode (wraps to first). Returns the newly active mode.
    ///
    /// Called when the user presses Shift+Tab.
    pub fn cycle(&mut self) -> &AgentMode {
        let current_name = self.registry.all_modes()[self.active_idx].name.clone();
        let next = self.registry.next_after(&current_name);
        // Resolve the index of the returned mode.
        let next_name = next.name.clone();
        self.active_idx = self.registry.all_modes()
            .iter()
            .position(|m| m.name == next_name)
            .expect("next_after returned a mode not in the registry");
        &self.registry.all_modes()[self.active_idx]
    }

    /// Set the active mode by name. Returns `Err(ModeError::UnknownMode)`
    /// if the name is not in the registry.
    pub fn set(&mut self, name: &str) -> Result<&AgentMode, ModeError> {
        let idx = self.registry.all_modes()
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| ModeError::UnknownMode(name.to_string()))?;
        self.active_idx = idx;
        Ok(&self.registry.all_modes()[self.active_idx])
    }

    /// Apply the active mode to `request` in place before the request is
    /// dispatched to `XaftRuntime`.
    ///
    /// Two fields of `RunRequest` are modified:
    ///
    /// 1. `mode_system_patch` — set to `Some(patch)` when the active mode's
    ///    `system_patch` is non-empty; `None` otherwise (runtime skips the
    ///    prepend).
    /// 2. `mode_tool_filter` — set to `Some(filter_fn)` when the active mode
    ///    has a tool filter; `None` otherwise (runtime allows all tools).
    pub fn apply_to_run_request(&self, request: &mut RunRequest) {
        let mode = self.active();

        request.mode_system_patch = if mode.system_patch.is_empty() {
            None
        } else {
            Some(mode.system_patch.clone())
        };

        request.mode_tool_filter = mode.tool_filter.clone();
    }

    /// Expose the registry for inspection (e.g., for the `/mode` slash command
    /// to list available modes).
    pub fn registry(&self) -> &ModeRegistry {
        &self.registry
    }

    /// Register a new mode at runtime (used by skill loader).
    pub fn register_mode(&mut self, mode: AgentMode) {
        self.registry.register(mode);
    }

    /// Remove all modes from `source_id` (used by skill unloader).
    ///
    /// If the active mode is removed, the active index is reset to 0
    /// (the "auto" builtin, which is always present).
    pub fn unregister_source(&mut self, source_id: &str) {
        let active_name = self.active().name.clone();
        self.registry.unregister_source(source_id);

        // Check whether the active mode survived; if not, reset to first.
        if self.registry.get(&active_name).is_none() {
            self.active_idx = 0;
        } else {
            // Rebuild active_idx because positions may have shifted.
            self.active_idx = self.registry.all_modes()
                .iter()
                .position(|m| m.name == active_name)
                .unwrap_or(0);
        }
    }
}

impl Default for ModeManager {
    fn default() -> Self {
        Self::default_builtin()
    }
}
```

---

## 4. `RunRequest` extension

### 4.1 Location

`crates/xaft-runtime/src/dispatch.rs` — the `RunRequest` struct.

### 4.2 New fields

```rust
use xaft_tui::mode::ToolFilterFn;   // or re-export path — see §8

/// Request to run a task.
#[derive(Debug, Clone)]
pub struct RunRequest {
    // ... all existing fields unchanged ...

    /// System prompt prefix injected by the active TUI mode.
    ///
    /// When `Some`, the orchestrator prepends this string to each agent's
    /// base system prompt before executing any LLM call. When `None`, no
    /// patch is applied and behaviour is identical to the pre-mode-system
    /// baseline.
    ///
    /// Set by `ModeManager::apply_to_run_request()` at submit time.
    pub mode_system_patch: Option<String>,

    /// Tool name filter from the active TUI mode.
    ///
    /// When `Some(f)`, the orchestrator calls `f(tool_name)` for each tool
    /// before registering it. Tools for which `f` returns `false` are
    /// excluded from all agent tool registries for this run. When `None`,
    /// all tools are allowed (default behaviour).
    ///
    /// Set by `ModeManager::apply_to_run_request()` at submit time.
    pub mode_tool_filter: Option<ToolFilterFn>,
}
```

**Dependency concern.** `RunRequest` lives in `xaft-runtime`, and `ToolFilterFn` is defined in `xaft-tui`. Since `xaft-runtime` must not depend on `xaft-tui` (architecture invariant), there are two clean options:

**Option A (recommended):** Extract `ToolFilterFn` to a new micro-crate `xaft-mode-types` (or into `xaft-agent` which is already depended upon by the runtime) and import it from both `xaft-runtime` and `xaft-tui`.

**Option B:** Define `ToolFilterFn` locally in `xaft-runtime` (as a type alias for `Arc<dyn Fn(&str) -> bool + Send + Sync>`) and re-alias it in `xaft-tui`. This avoids a new crate but duplicates the definition.

This PRD recommends **Option A**: add a `xaft-mode-types` crate containing only the type aliases (`ToolFilterFn`, `PreHookFn`, `PostHookFn`) and import it where needed. The crate has no further dependencies.

For the purposes of this PRD the type is written as if imported from `xaft_mode_types`:

```rust
// crates/xaft-mode-types/src/lib.rs
use std::sync::Arc;

pub type ToolFilterFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type PreHookFn    = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type PostHookFn   = Arc<dyn Fn(&str) -> String + Send + Sync>;
```

**Default values for new fields.** All existing `RunRequest` construction sites must be updated to add the two new fields. The easiest path is to add `Default` impls for the new fields and use struct-update syntax:

```rust
// In dispatch.rs alongside RunRequest
impl RunRequest {
    /// Convenience: construct with mode fields at their defaults.
    pub fn with_mode_defaults(mut self) -> Self {
        self.mode_system_patch = None;
        self.mode_tool_filter  = None;
        self
    }
}
```

All callers that currently construct `RunRequest { ... }` with named fields (e.g., in tests, in `resume_session`) must add:

```rust
mode_system_patch: None,
mode_tool_filter: None,
```

---

## 5. Runtime integration

### 5.1 Orchestrator — system prompt patch

Location: `crates/xaft-runtime/src/orchestrator.rs`

`run_workflow`, `run_dynamic_handoff`, and `run_meta_workflow` all receive the full `RunRequest`. The system patch must be prepended to every agent's system prompt.

The existing prompt-building path is:

```rust
// xaft-agents/src/planner.rs (approx)
let system = planner_system_prompt(/* config options */);
```

The new path inserts an optional prefix before the role-specific text:

```rust
fn prepend_mode_patch(base: &str, patch: Option<&str>) -> String {
    match patch {
        Some(p) if !p.trim().is_empty() => format!("{p}\n\n{base}"),
        _ => base.to_string(),
    }
}
```

Usage in each orchestrated agent build:

```rust
let planner_system = prepend_mode_patch(
    &planner_system_prompt(/* ... */),
    request.mode_system_patch.as_deref(),
);
let coder_system = prepend_mode_patch(
    &coder_system_prompt(/* ... */),
    request.mode_system_patch.as_deref(),
);
// ... qa, fixer similarly
```

The helper `prepend_mode_patch` should be a private free function in `orchestrator.rs`.

### 5.2 Orchestrator — tool filter

The tool filter is applied when the tool vectors are assembled. The existing code in `runtime.rs` (`run_task`) builds `read_tools` and `write_tools` as `Vec<Arc<ErasedTool>>`. The filter is applied after assembly:

```rust
// In run_task, after read_tools and write_tools are built:
if let Some(ref filter) = request.mode_tool_filter {
    read_tools.retain(|t| filter(t.name()));
    write_tools.retain(|t| filter(t.name()));
}
```

`ErasedTool::name()` returns the snake_case tool name string (the same string the LLM uses). This is already available on the `ErasedTool` trait.

This single application point is sufficient: all orchestration paths (`run_workflow`, `run_dynamic_handoff`, `run_meta_workflow`) receive `read_tools` and `write_tools` as function arguments, so filtering at the call site in `run_task` covers all paths without duplicating the logic.

### 5.3 Pre/post hooks

Pre- and post-hooks are applied in `AppState`, not in the runtime. This keeps the runtime free of TUI concerns:

- **Pre-hook**: called in `handle_submit()` after slash-command interception and before the `UserMessage` is sent to the runtime channel. The modified text replaces the original.
- **Post-hook**: called in the `TuiEvent::AgentOutput` arm of `handle_event()`, after `markdown_renderer.render(&content)` and before lines are committed to the transcript.

```rust
// In handle_submit(), after slash-command handling, before send_expanded():
let text = if let Some(ref hook) = self.mode_manager.active().pre_hook {
    hook(&msg)
} else {
    msg
};
// proceed with text instead of msg

// In handle_event() → TuiEvent::AgentOutput:
let content = if let Some(ref hook) = self.mode_manager.active().post_hook {
    hook(&content)
} else {
    content
};
let rendered = self.markdown_renderer.render(&content);
```

---

## 6. `XaftModeChanged` signal

### 6.1 Definition

Add to `crates/xaft-agent/src/signals.rs`:

```rust
// ── Mode system signal ────────────────────────────────────────────────────────

/// Emitted by `AppState` when the active mode changes — either via
/// Shift+Tab cycling or `/mode <name>`.
///
/// Observational only: does not gate any workflow transition.
#[derive(Debug, Clone)]
pub struct XaftModeChanged {
    /// Name of the mode that was active before the change.
    pub from_name: String,
    /// Name of the newly active mode.
    pub to_name: String,
    /// Short label of the newly active mode (e.g. `"PLAN"`).
    pub to_label: String,
    /// Colour name of the newly active mode (e.g. `"cyan"`).
    pub to_colour: String,
}
```

### 6.2 Emission

`AppState` emits this signal whenever `mode_manager.cycle()` or `mode_manager.set()` succeeds:

```rust
fn emit_mode_changed(state: &AppState, from_name: &str) {
    let mode = state.mode_manager.active();
    let signal = XaftModeChanged {
        from_name: from_name.to_string(),
        to_name:   mode.name.clone(),
        to_label:  mode.label.clone(),
        to_colour: mode.colour.as_str().to_string(),
    };
    emit_signal_if_bus(state.signal_bus.clone(), signal);
}
```

Call site in `AppState::handle_key()`:

```rust
KeyCode::BackTab => {
    // Shift+Tab — cycle mode.
    let from_name = self.mode_manager.active().name.clone();
    self.mode_manager.cycle();
    emit_mode_changed(self, &from_name);
    self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
}
```

Call site in `/mode <name>` slash command handler:

```rust
let from_name = state.mode_manager.active().name.clone();
match state.mode_manager.set(&args) {
    Ok(_) => {
        emit_mode_changed(state, &from_name);
        state.mutations.push(RenderMutation::UpdatePrompt(build_prompt(state)));
        CommandResult::Lines(vec![
            format!("Mode set to {}", state.mode_manager.active().label)
        ])
    }
    Err(ModeError::UnknownMode(n)) => {
        CommandResult::Error(format!("Unknown mode: {n}"))
    }
    Err(e) => CommandResult::Error(e.to_string()),
}
```

---

## 7. `AppState` integration

### 7.1 New field

```rust
// In AppState struct (state.rs):
/// Active mode manager — drives Shift+Tab cycling and /mode command.
pub mode_manager: ModeManager,
```

Initialised in `AppState::new()`:

```rust
mode_manager: ModeManager::default_builtin(),
```

### 7.2 Prompt badge

`build_prompt()` in `prompt.rs` reads the active mode to render the badge:

```rust
// In build_prompt(state: &AppState) -> PromptState:
let mode = state.mode_manager.active();
let mode_badge = format!("[{}]", mode.label);
let badge_colour = mode.colour; // consumed by the prompt renderer
```

The prompt line rendered by `format_prompt_line()` includes the badge between the working-dir display and the input cursor. The badge is only shown when the mode is not "auto" OR always — implementation detail left to the implementor; the badge must always be rendered for discoverability.

### 7.3 Task submission path

In `handle_submit()`, after slash interception and before `send_text_and_reset` / `send_expanded`:

```rust
// Apply pre-hook if present.
let msg = if let Some(ref hook) = self.mode_manager.active().pre_hook {
    hook(&msg)
} else {
    msg
};

// Build the RunRequest (existing path) and apply mode fields.
// Note: RunRequest construction happens inside TuiApp::run_loop(), not
// inside handle_submit(). AppState signals via user_message_tx that a
// message is ready; TuiApp assembles the RunRequest and calls
// mode_manager.apply_to_run_request(&mut req) before dispatching.
```

The flow is:

```
User presses Enter
  → AppState::handle_submit()
      → pre_hook applied (if any) → UserMessage sent via user_message_tx
  → TuiApp::run_loop() receives UserMessage from channel
      → builds RunRequest { task, config, working_dir, ... }
      → calls state.mode_manager.apply_to_run_request(&mut req)
      → dispatches req to XaftRuntime::run()
```

`apply_to_run_request` is called in `TuiApp`'s message-receive arm, not inside `AppState` itself, because `AppState` does not own the `XaftRuntime` or construct `RunRequest` directly. `AppState` communicates with `TuiApp` via the `user_message_tx` channel, and `TuiApp` holds `mode_manager_ref: Arc<RwLock<ModeManager>>` (or, simpler, reads the active mode out of a shared reference before constructing the request).

**Simpler approach**: because `TuiApp` and `AppState` run on the same thread (the main event loop), `TuiApp` can call `state.mode_manager.apply_to_run_request(&mut req)` directly after constructing the `RunRequest` and before spawning the runtime task. No locking needed.

---

## 8. File layout

```
crates/xaft-tui/src/mode/
    mod.rs          ← AgentMode, ModeColour, ToolFilterFn, PreHookFn, PostHookFn,
                       AgentModeBuilder, builtin_auto(), builtin_plan(), builtin_safe()
    registry.rs     ← ModeRegistry
    manager.rs      ← ModeManager, ModeError

crates/xaft-mode-types/src/
    lib.rs          ← ToolFilterFn, PreHookFn, PostHookFn (type aliases only)

crates/xaft-runtime/src/
    dispatch.rs     ← RunRequest gains mode_system_patch, mode_tool_filter
    runtime.rs      ← apply mode_tool_filter after tool build

crates/xaft-agent/src/
    signals.rs      ← XaftModeChanged added

crates/xaft-tui/src/
    lib.rs          ← re-export ModeManager, ModeRegistry, AgentMode,
                       ModeColour, ModeError, AgentModeBuilder
    state.rs        ← AppState gains mode_manager field
    prompt.rs       ← build_prompt reads active mode for badge
    slash/commands/ ← /mode <name> slash command handler added
```

### 8.1 Re-exports in `xaft-tui/src/lib.rs`

Add:

```rust
pub mod mode;

pub use mode::{
    AgentMode, AgentModeBuilder, ModeColour,
    ToolFilterFn, PreHookFn, PostHookFn,
};
pub use mode::manager::{ModeManager, ModeError};
pub use mode::registry::ModeRegistry;
```

### 8.2 `/mode` slash command

Add `SlashCommand::Mode { name: Option<String> }` to the existing `SlashCommand` enum in `slash/mod.rs`. When `name` is `None`, list available modes. When `Some`, set the mode.

```rust
SlashCommand::Mode { name: None } → list modes with labels, colours, descriptions
SlashCommand::Mode { name: Some(n) } → state.mode_manager.set(&n)
```

---

## 9. Tests

### 9.1 Unit tests in `mod.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mode_badge_contains_label() {
        let mode = AgentModeBuilder::new("auto", "AUTO")
            .colour(ModeColour::Green)
            .build();
        // Simulate prompt formatting (the real formatter uses mode.label).
        let badge = format!("[{}]", mode.label);
        assert!(badge.contains("AUTO"));
    }

    #[test]
    fn agent_mode_builder_defaults_source_id_to_builtin() {
        let mode = AgentModeBuilder::new("x", "X").build();
        assert_eq!(mode.source_id, "builtin");
    }

    #[test]
    fn agent_mode_has_no_hooks_by_default() {
        let mode = AgentModeBuilder::new("x", "X").build();
        assert!(mode.tool_filter.is_none());
        assert!(mode.pre_hook.is_none());
        assert!(mode.post_hook.is_none());
    }

    #[test]
    fn tool_filter_fn_is_callable() {
        let filter: ToolFilterFn = Arc::new(|name: &str| name.starts_with("read"));
        assert!(filter("read_file"));
        assert!(!filter("write_file"));
    }

    #[test]
    fn pre_hook_modifies_text() {
        let hook: PreHookFn = Arc::new(|text: &str| format!("[debug] {text}"));
        let result = hook("hello world");
        assert_eq!(result, "[debug] hello world");
    }

    #[test]
    fn mode_colour_as_str() {
        assert_eq!(ModeColour::Green.as_str(), "green");
        assert_eq!(ModeColour::Cyan.as_str(), "cyan");
        assert_eq!(ModeColour::Yellow.as_str(), "yellow");
    }
}
```

### 9.2 Unit tests in `registry.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{AgentModeBuilder, ModeColour};

    fn make_mode(name: &str, source: &str) -> AgentMode {
        AgentModeBuilder::new(name, &name.to_uppercase())
            .source_id(source)
            .build()
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("auto", "builtin"));
        let m = reg.get("auto").expect("mode should exist");
        assert_eq!(m.name, "auto");
        assert_eq!(m.label, "AUTO");
    }

    #[test]
    fn registry_register_replaces_existing() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("auto", "builtin"));
        // Register a second mode with the same name but different label.
        let mut m2 = make_mode("auto", "builtin");
        m2.label = "AUTOPLUS".to_string();
        reg.register(m2);
        // Still one mode, updated label.
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("auto").unwrap().label, "AUTOPLUS");
    }

    #[test]
    fn registry_cycle_order_wraps() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("auto", "builtin"));
        reg.register(make_mode("plan", "builtin"));
        reg.register(make_mode("safe", "builtin"));

        // auto → plan
        assert_eq!(reg.next_after("auto").name, "plan");
        // plan → safe
        assert_eq!(reg.next_after("plan").name, "safe");
        // safe → auto (wraps)
        assert_eq!(reg.next_after("safe").name, "auto");
    }

    #[test]
    fn registry_unregister_source_removes_only_matching() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("auto", "builtin"));
        reg.register(make_mode("plan", "builtin"));
        reg.register(make_mode("debug", "skill:debug_plugin"));
        reg.register(make_mode("verbose", "skill:debug_plugin"));

        reg.unregister_source("skill:debug_plugin");

        // Builtins survive.
        assert!(reg.get("auto").is_some());
        assert!(reg.get("plan").is_some());
        // Plugin modes gone.
        assert!(reg.get("debug").is_none());
        assert!(reg.get("verbose").is_none());
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn registry_all_modes_preserves_insertion_order() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("c", "builtin"));
        reg.register(make_mode("a", "builtin"));
        reg.register(make_mode("b", "builtin"));
        let names: Vec<&str> = reg.all_modes().iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    #[test]
    fn default_registry_has_three_builtins() {
        let reg = ModeRegistry::default();
        assert_eq!(reg.len(), 3);
        assert!(reg.get("auto").is_some());
        assert!(reg.get("plan").is_some());
        assert!(reg.get("safe").is_some());
    }
}
```

### 9.3 Unit tests in `manager.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{AgentModeBuilder, ModeRegistry};

    fn three_mode_manager() -> ModeManager {
        let mut reg = ModeRegistry::new();
        reg.register(AgentModeBuilder::new("auto", "AUTO").source_id("builtin").build());
        reg.register(AgentModeBuilder::new("plan", "PLAN").source_id("builtin")
            .system_patch("plan patch").build());
        reg.register(AgentModeBuilder::new("safe", "SAFE").source_id("builtin").build());
        ModeManager::new(reg).unwrap()
    }

    #[test]
    fn manager_cycle_advances_mode() {
        let mut mm = three_mode_manager();
        assert_eq!(mm.active().name, "auto");
        mm.cycle();
        assert_eq!(mm.active().name, "plan");
        mm.cycle();
        assert_eq!(mm.active().name, "safe");
        mm.cycle(); // wraps
        assert_eq!(mm.active().name, "auto");
    }

    #[test]
    fn manager_set_by_name() {
        let mut mm = three_mode_manager();
        let mode = mm.set("plan").unwrap();
        assert_eq!(mode.name, "plan");
        assert_eq!(mm.active().name, "plan");
    }

    #[test]
    fn manager_set_unknown_returns_err() {
        let mut mm = three_mode_manager();
        let err = mm.set("nonexistent").unwrap_err();
        assert!(matches!(err, ModeError::UnknownMode(_)));
        let ModeError::UnknownMode(name) = err else { panic!() };
        assert_eq!(name, "nonexistent");
    }

    #[test]
    fn manager_apply_system_patch_prepends() {
        use std::path::PathBuf;
        use xaft_config::XaftConfig;
        use xaft_runtime::dispatch::RunRequest;
        use xaft_runtime::agent_registry::WorkflowConfig;

        let mut mm = three_mode_manager();
        mm.set("plan").unwrap();

        let mut req = RunRequest {
            task: "do something".to_string(),
            config: XaftConfig::default(),
            working_dir: PathBuf::from("."),
            headless: true,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
            resume_session_id: None,
            workflow: WorkflowConfig::default(),
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
        };

        mm.apply_to_run_request(&mut req);

        let patch = req.mode_system_patch.expect("plan mode must set a patch");
        assert!(patch.contains("plan patch"));
    }

    #[test]
    fn manager_apply_tool_filter_restricts() {
        use std::sync::Arc;
        use std::path::PathBuf;
        use xaft_config::XaftConfig;
        use xaft_runtime::dispatch::RunRequest;
        use xaft_runtime::agent_registry::WorkflowConfig;

        let mut reg = ModeRegistry::new();
        reg.register(
            AgentModeBuilder::new("read_only", "RO")
                .tool_filter(Arc::new(|name: &str| {
                    matches!(name, "read_file" | "list_dir" | "grep_in_file")
                }))
                .source_id("builtin")
                .build(),
        );
        let mut mm = ModeManager::new(reg).unwrap();

        let mut req = RunRequest {
            task: "x".to_string(),
            config: XaftConfig::default(),
            working_dir: PathBuf::from("."),
            headless: true,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
            resume_session_id: None,
            workflow: WorkflowConfig::default(),
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
        };

        mm.apply_to_run_request(&mut req);

        let filter = req.mode_tool_filter.expect("read_only mode must set filter");
        assert!(filter("read_file"));
        assert!(filter("list_dir"));
        assert!(!filter("write_file"));
        assert!(!filter("bash_exec"));
        assert!(!filter("git_push"));
    }

    #[test]
    fn manager_no_filter_allows_all() {
        let mut mm = three_mode_manager(); // "auto" has no filter
        assert_eq!(mm.active().name, "auto");

        let mut req = make_empty_request();
        mm.apply_to_run_request(&mut req);

        // auto mode → no patch, no filter.
        assert!(req.mode_system_patch.is_none());
        assert!(req.mode_tool_filter.is_none());
    }

    #[test]
    fn manager_unregister_source_resets_active_if_removed() {
        let mut reg = ModeRegistry::new();
        reg.register(AgentModeBuilder::new("auto", "AUTO").source_id("builtin").build());
        reg.register(AgentModeBuilder::new("plugin_mode", "PM").source_id("skill:foo").build());
        let mut mm = ModeManager::new(reg).unwrap();

        mm.set("plugin_mode").unwrap();
        assert_eq!(mm.active().name, "plugin_mode");

        mm.unregister_source("skill:foo");
        // Active must fall back to first available mode.
        assert_eq!(mm.active().name, "auto");
    }

    fn make_empty_request() -> xaft_runtime::dispatch::RunRequest {
        use std::path::PathBuf;
        use xaft_config::XaftConfig;
        use xaft_runtime::agent_registry::WorkflowConfig;
        xaft_runtime::dispatch::RunRequest {
            task: String::new(),
            config: XaftConfig::default(),
            working_dir: PathBuf::from("."),
            headless: true,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
            resume_session_id: None,
            workflow: WorkflowConfig::default(),
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
        }
    }
}
```

### 9.4 Integration tests

File: `crates/xaft-tui/tests/mode_system_integration.rs`

```rust
//! Integration tests for the mode system end-to-end.
//!
//! Tests AppState integration: mode cycling, /mode command, prompt badge
//! update, and RunRequest patching.

use std::sync::Arc;
use xaft_tui::mode::{AgentModeBuilder, ModeColour, ModeRegistry};
use xaft_tui::mode::manager::{ModeError, ModeManager};
use xaft_tui::AppState;

// ── Helper ────────────────────────────────────────────────────────────────────

fn three_mode_manager() -> ModeManager {
    let mut reg = ModeRegistry::new();
    reg.register(
        AgentModeBuilder::new("auto", "AUTO")
            .colour(ModeColour::Green)
            .source_id("builtin")
            .build(),
    );
    reg.register(
        AgentModeBuilder::new("plan", "PLAN")
            .colour(ModeColour::Cyan)
            .system_patch("plan patch text")
            .source_id("builtin")
            .build(),
    );
    reg.register(
        AgentModeBuilder::new("safe", "SAFE")
            .colour(ModeColour::Yellow)
            .source_id("builtin")
            .build(),
    );
    ModeManager::new(reg).unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn mode_badge_contains_label() {
    let mm = three_mode_manager();
    let badge = format!("[{}]", mm.active().label);
    assert!(badge.contains("AUTO"), "badge '{badge}' should contain AUTO");
}

#[test]
fn registry_register_and_get() {
    let mut reg = ModeRegistry::new();
    reg.register(AgentModeBuilder::new("auto", "AUTO").build());
    assert!(reg.get("auto").is_some());
    assert!(reg.get("unknown").is_none());
}

#[test]
fn registry_cycle_order_wraps() {
    let mut reg = ModeRegistry::new();
    reg.register(AgentModeBuilder::new("auto", "AUTO").build());
    reg.register(AgentModeBuilder::new("plan", "PLAN").build());
    reg.register(AgentModeBuilder::new("safe", "SAFE").build());

    assert_eq!(reg.next_after("auto").name, "plan");
    assert_eq!(reg.next_after("plan").name, "safe");
    assert_eq!(reg.next_after("safe").name, "auto");
}

#[test]
fn registry_unregister_source_removes_only_matching() {
    let mut reg = ModeRegistry::new();
    reg.register(AgentModeBuilder::new("auto", "AUTO").source_id("builtin").build());
    reg.register(AgentModeBuilder::new("ext", "EXT").source_id("skill:myplugin").build());
    reg.unregister_source("skill:myplugin");
    assert!(reg.get("auto").is_some());
    assert!(reg.get("ext").is_none());
}

#[test]
fn manager_cycle_advances_mode() {
    let mut mm = three_mode_manager();
    assert_eq!(mm.active().name, "auto");
    mm.cycle();
    assert_eq!(mm.active().name, "plan");
    mm.cycle();
    assert_eq!(mm.active().name, "safe");
    mm.cycle();
    assert_eq!(mm.active().name, "auto");
}

#[test]
fn manager_set_by_name() {
    let mut mm = three_mode_manager();
    mm.set("plan").unwrap();
    assert_eq!(mm.active().name, "plan");
    assert_eq!(mm.active().label, "PLAN");
}

#[test]
fn manager_set_unknown_returns_err() {
    let mut mm = three_mode_manager();
    let result = mm.set("does_not_exist");
    assert!(
        matches!(result, Err(ModeError::UnknownMode(_))),
        "expected UnknownMode, got: {:?}",
        result
    );
}

#[test]
fn manager_apply_system_patch_prepends() {
    use std::path::PathBuf;
    use xaft_config::XaftConfig;
    use xaft_runtime::dispatch::RunRequest;
    use xaft_runtime::agent_registry::WorkflowConfig;

    let mut mm = three_mode_manager();
    mm.set("plan").unwrap();

    let mut req = RunRequest {
        task: "test task".into(),
        config: XaftConfig::default(),
        working_dir: PathBuf::from("."),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    };

    mm.apply_to_run_request(&mut req);

    let patch = req.mode_system_patch.expect("plan mode sets a patch");
    assert!(
        patch.contains("plan patch text"),
        "patch should contain the mode's system_patch text, got: '{patch}'"
    );
}

#[test]
fn manager_apply_tool_filter_restricts() {
    use xaft_tui::mode::ToolFilterFn;
    use xaft_runtime::dispatch::RunRequest;
    use std::path::PathBuf;
    use xaft_config::XaftConfig;
    use xaft_runtime::agent_registry::WorkflowConfig;

    let mut reg = ModeRegistry::new();
    let filter: ToolFilterFn = Arc::new(|name: &str| name.starts_with("read"));
    reg.register(
        AgentModeBuilder::new("read_only", "RO")
            .tool_filter(filter)
            .source_id("builtin")
            .build(),
    );
    let mut mm = ModeManager::new(reg).unwrap();

    let mut req = RunRequest {
        task: "x".into(),
        config: XaftConfig::default(),
        working_dir: PathBuf::from("."),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    };

    mm.apply_to_run_request(&mut req);

    let f = req.mode_tool_filter.expect("read_only mode sets filter");
    assert!(f("read_file"));
    assert!(f("read_url"));
    assert!(!f("write_file"));
    assert!(!f("bash_exec"));
}

#[test]
fn manager_no_filter_allows_all() {
    use xaft_runtime::dispatch::RunRequest;
    use std::path::PathBuf;
    use xaft_config::XaftConfig;
    use xaft_runtime::agent_registry::WorkflowConfig;

    let mm = three_mode_manager(); // "auto" is active, has no filter

    let mut req = RunRequest {
        task: "x".into(),
        config: XaftConfig::default(),
        working_dir: PathBuf::from("."),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    };

    mm.apply_to_run_request(&mut req);

    assert!(req.mode_system_patch.is_none(), "auto mode must not set a patch");
    assert!(req.mode_tool_filter.is_none(), "auto mode must not set a filter");
}
```

---

## 10. Key differences from agenthicc PRD-47

> **Callout: Rust vs Python differences**
>
> | Aspect | Python (PRD-47) | Rust (PRD-64) |
> |--------|----------------|---------------|
> | Hook / filter type | `Callable[[str], str]` (any callable) | `Arc<dyn Fn(&str) -> String + Send + Sync>` — explicit Send+Sync required for cross-thread use |
> | `ToolFilterFn` signature | `Callable[[str, dict], bool]` (name + context dict) | `Fn(&str) -> bool` — name only, no context dict; context dict not needed because the filter is applied at run-request construction time, not inside the agent turn |
> | Mode application timing | Inside `_run_agent_turn` each LLM call | At `RunRequest` construction time in `TuiApp`, before the request reaches `XaftRuntime` — the runtime sees the patch and filter as plain fields |
> | Error type | Python `ValueError` / `KeyError` | `thiserror` typed `ModeError` enum; never `anyhow` in library code |
> | Runtime reflection | Python can introspect callables at runtime | Rust type system provides no callable introspection; the `Debug` impl for `AgentMode` records `has_tool_filter: bool` as the only observable |
> | Thread safety | GIL protects shared state | `Arc` + `Send + Sync` bounds are mandatory; all closures must be `Send + Sync` |
> | Registry mutability | Python list append; thread-safe via asyncio | `ModeRegistry` is `!Send` on its own; `ModeManager` lives in `AppState` which is single-threaded (main event loop); no locking needed |

---

## 11. Dependency graph summary

```
xaft-mode-types  (new, no deps)
     ↑               ↑
xaft-runtime    xaft-tui/mode
(RunRequest)    (AgentMode, ModeManager)
```

`xaft-runtime` depends on `xaft-mode-types` for `ToolFilterFn`.
`xaft-tui` depends on `xaft-mode-types` for the same type alias.
`xaft-tui` does NOT depend on `xaft-runtime` (existing invariant preserved).
The binary layer (`xaft`) wires `TuiApp` → `XaftRuntime`, so the concrete `RunRequest` construction with mode fields happens there.

---

## 12. Migration checklist for the implementor

1. Create `crates/xaft-mode-types/` with `Cargo.toml` + `src/lib.rs` (type aliases only).
2. Add `xaft-mode-types` as a dependency in `xaft-runtime/Cargo.toml` and `xaft-tui/Cargo.toml`.
3. Create `crates/xaft-tui/src/mode/` directory with `mod.rs`, `registry.rs`, `manager.rs`.
4. Add `pub mod mode;` to `xaft-tui/src/lib.rs` and add re-exports (§8.1).
5. Add `mode_system_patch` and `mode_tool_filter` to `RunRequest` in `dispatch.rs`. Update all construction sites to include the new fields (grep for `RunRequest {`).
6. Add `mode_manager: ModeManager` field to `AppState::new()`.
7. Add `XaftModeChanged` to `crates/xaft-agent/src/signals.rs`.
8. Update `orchestrator.rs`/`runtime.rs` to apply the patch and filter (§5).
9. Add `SlashCommand::Mode` variant and handler (§6.2, §8.2).
10. Update `build_prompt` to include mode badge.
11. Run all tests: `cargo test --workspace`.
12. Run `cargo clippy --workspace` — pay attention to `Arc<dyn Fn>` lint suggestions.
