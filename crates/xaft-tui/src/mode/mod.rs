//! Mode system data layer for xaft TUI.
//!
//! `AgentMode` carries a name, colour, optional system prompt patch,
//! an optional tool filter, and optional pre/post hooks.

use std::sync::Arc;

// Re-export the canonical type alias from xaft-runtime so there is only
// one definition and no type mismatch when assigning to RunRequest.
pub use xaft_runtime::dispatch::ModeToolFilter as ToolFilterFn;

/// Pre-hook: transforms the user message before it is sent to the runtime.
pub type PreHookFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
/// Post-hook: transforms agent output before it is appended to the transcript.
pub type PostHookFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Named colour for a mode's badge in the prompt.
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
    /// ANSI escape code for the foreground colour.
    pub fn ansi_code(&self) -> &'static str {
        match self {
            Self::Green => "\x1b[32m",
            Self::Yellow => "\x1b[33m",
            Self::Cyan => "\x1b[36m",
            Self::Blue => "\x1b[34m",
            Self::Magenta => "\x1b[35m",
            Self::Red => "\x1b[31m",
            Self::White => "\x1b[37m",
        }
    }

    /// Lower-case ASCII name for use in signals and config.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Cyan => "cyan",
            Self::Blue => "blue",
            Self::Magenta => "magenta",
            Self::Red => "red",
            Self::White => "white",
        }
    }
}

/// A named agent mode that modifies runtime behaviour.
pub struct AgentMode {
    /// Unique programmatic identifier (e.g. `"auto"`, `"plan"`).
    pub name: String,
    /// Short display label shown in the prompt badge (e.g. `"AUTO"`, `"PLAN"`).
    pub label: String,
    /// One-sentence description shown in `/mode` help.
    pub description: String,
    /// Colour of the prompt badge.
    pub colour: ModeColour,
    /// System prompt prefix prepended before the agent's base prompt.
    /// Empty string means "no patch".
    pub system_patch: String,
    /// Optional tool filter — `None` means "allow all tools".
    pub tool_filter: Option<ToolFilterFn>,
    /// Optional pre-hook: transforms the user message before sending.
    pub pre_hook: Option<PreHookFn>,
    /// Optional post-hook: transforms agent output before displaying.
    pub post_hook: Option<PostHookFn>,
    /// ID of the source that registered this mode (e.g. `"builtin"`, MCP server name).
    pub source_id: String,
}

impl AgentMode {
    /// Render a coloured ANSI badge: `[LABEL]`.
    pub fn ansi_badge(&self) -> String {
        format!("{}[{}]\x1b[0m", self.colour.ansi_code(), self.label)
    }
}

impl std::fmt::Debug for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMode")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("colour", &self.colour)
            .field("system_patch_len", &self.system_patch.len())
            .field("has_tool_filter", &self.tool_filter.is_some())
            .field("has_pre_hook", &self.pre_hook.is_some())
            .field("has_post_hook", &self.post_hook.is_some())
            .field("source_id", &self.source_id)
            .finish()
    }
}

// ── Builder ────────────────────────────────────────────────────────────────────

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
    /// Start building a new mode with `name` and `label`.
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
            source_id: "builtin".into(),
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

    pub fn system_patch(mut self, p: impl Into<String>) -> Self {
        self.system_patch = p.into();
        self
    }

    pub fn tool_filter(mut self, f: ToolFilterFn) -> Self {
        self.tool_filter = Some(f);
        self
    }

    pub fn pre_hook(mut self, h: PreHookFn) -> Self {
        self.pre_hook = Some(h);
        self
    }

    pub fn post_hook(mut self, h: PostHookFn) -> Self {
        self.post_hook = Some(h);
        self
    }

    pub fn source_id(mut self, s: impl Into<String>) -> Self {
        self.source_id = s.into();
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

pub mod builtins;
pub mod manager;
pub mod registry;

pub use builtins::{build_default_mode_registry, builtin_modes};
pub use manager::{ModeError, ModeManager};
pub use registry::ModeRegistry;

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_sets_all_fields() {
        let mode = AgentModeBuilder::new("test", "TEST")
            .description("A test mode")
            .colour(ModeColour::Cyan)
            .system_patch("patch text")
            .source_id("custom")
            .build();
        assert_eq!(mode.name, "test");
        assert_eq!(mode.label, "TEST");
        assert_eq!(mode.description, "A test mode");
        assert_eq!(mode.colour, ModeColour::Cyan);
        assert_eq!(mode.system_patch, "patch text");
        assert_eq!(mode.source_id, "custom");
        assert!(mode.tool_filter.is_none());
        assert!(mode.pre_hook.is_none());
        assert!(mode.post_hook.is_none());
    }

    #[test]
    fn ansi_badge_format() {
        let mode = AgentModeBuilder::new("auto", "AUTO")
            .colour(ModeColour::Green)
            .build();
        let badge = mode.ansi_badge();
        assert!(badge.contains("[AUTO]"));
        assert!(badge.contains("\x1b[32m"));
    }

    #[test]
    fn mode_colour_as_str() {
        assert_eq!(ModeColour::Green.as_str(), "green");
        assert_eq!(ModeColour::Yellow.as_str(), "yellow");
        assert_eq!(ModeColour::Red.as_str(), "red");
    }

    #[test]
    fn tool_filter_works() {
        let allowed = vec!["read_file".to_string(), "grep".to_string()];
        let filter: ToolFilterFn = Arc::new(move |name: &str| allowed.contains(&name.to_string()));
        let mode = AgentModeBuilder::new("plan", "PLAN")
            .tool_filter(filter)
            .build();
        let f = mode.tool_filter.as_ref().unwrap();
        assert!(f("read_file"));
        assert!(!f("write_file"));
    }

    #[test]
    fn debug_impl_does_not_panic() {
        let mode = AgentModeBuilder::new("test", "T").build();
        let _ = format!("{mode:?}");
    }
}
