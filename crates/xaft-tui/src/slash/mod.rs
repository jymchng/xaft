//! Slash command system for the xaft TUI.
//!
//! Intercepts `/`-prefixed input before it reaches the agent and dispatches
//! to built-in handlers.

pub mod commands;
pub mod palette;
pub mod parser;
pub mod registry;
pub mod table;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::signals::SignalBus;
use xaft_config::XaftConfig;
use xaft_runtime::session_store::SessionStore;

use crate::transcript::StyledLine;

// ── SlashCommand enum ─────────────────────────────────────────────────────────

/// Parsed slash command with any arguments pre-validated.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Help,
    Clear,
    Compact,
    Config,
    Cost,
    Init,
    Agents,
    Mcp,
    Resume {
        id: Option<String>,
    },
    Rewind {
        msg_index: Option<usize>,
    },
    Permissions,
    Model {
        name: String,
    },
    Vim,
    Emacs,
    Theme {
        name: Option<String>,
    },
    Login,
    Logout,
    Doctor,
    Memory,
    Diff,
    Commit,
    Pr {
        title: Option<String>,
    },
    Quit,
    /// Show or switch the active operational mode.
    Mode {
        name: Option<String>,
    },
}

impl SlashCommand {
    /// Return one example of each variant (used for registry validation and tests).
    pub fn all_example_variants() -> Vec<SlashCommand> {
        vec![
            SlashCommand::Help,
            SlashCommand::Clear,
            SlashCommand::Compact,
            SlashCommand::Config,
            SlashCommand::Cost,
            SlashCommand::Init,
            SlashCommand::Agents,
            SlashCommand::Mcp,
            SlashCommand::Resume { id: None },
            SlashCommand::Rewind { msg_index: None },
            SlashCommand::Permissions,
            SlashCommand::Model {
                name: String::new(),
            },
            SlashCommand::Vim,
            SlashCommand::Emacs,
            SlashCommand::Theme { name: None },
            SlashCommand::Login,
            SlashCommand::Logout,
            SlashCommand::Doctor,
            SlashCommand::Memory,
            SlashCommand::Diff,
            SlashCommand::Commit,
            SlashCommand::Pr { title: None },
            SlashCommand::Quit,
            SlashCommand::Mode { name: None },
        ]
    }

    /// Return the discriminants of all variants (used for registry validation).
    pub fn all_discriminants() -> Vec<std::mem::Discriminant<SlashCommand>> {
        Self::all_example_variants()
            .iter()
            .map(std::mem::discriminant)
            .collect()
    }

    /// The canonical trigger name (no leading slash).
    pub fn trigger_name(&self) -> &'static str {
        match self {
            SlashCommand::Help => "help",
            SlashCommand::Clear => "clear",
            SlashCommand::Compact => "compact",
            SlashCommand::Config => "config",
            SlashCommand::Cost => "cost",
            SlashCommand::Init => "init",
            SlashCommand::Agents => "agents",
            SlashCommand::Mcp => "mcp",
            SlashCommand::Resume { .. } => "resume",
            SlashCommand::Rewind { .. } => "rewind",
            SlashCommand::Permissions => "permissions",
            SlashCommand::Model { .. } => "model",
            SlashCommand::Vim => "vim",
            SlashCommand::Emacs => "emacs",
            SlashCommand::Theme { .. } => "theme",
            SlashCommand::Login => "login",
            SlashCommand::Logout => "logout",
            SlashCommand::Doctor => "doctor",
            SlashCommand::Memory => "memory",
            SlashCommand::Diff => "diff",
            SlashCommand::Commit => "commit",
            SlashCommand::Pr { .. } => "pr",
            SlashCommand::Quit => "quit",
            SlashCommand::Mode { .. } => "mode",
        }
    }
}

// ── CommandResult ─────────────────────────────────────────────────────────────

/// The result of executing a slash command.
pub enum CommandResult {
    /// Zero or more plaintext lines to print to the transcript.
    Lines(Vec<String>),
    /// Lines with explicit styling (for tables, errors, etc.).
    StyledLines(Vec<StyledLine>),
    /// The command pushed mutations directly; nothing more to do.
    Handled,
    /// The command failed; print an error line.
    Error(String),
    /// Formatted config display (read-only, section-grouped).
    ///
    /// Rendered as committed `StyledLine`s — no interactive navigation.
    /// The transcript is append-only; attempting key-based navigation on
    /// committed lines is architecturally impossible.
    ConfigDisplay(Vec<ConfigSection>),
    /// Open an interactive menu overlay driven by `MenuDriver`.
    ///
    /// `Box<dyn MenuWidget>` is not `Clone` — this variant must be consumed
    /// exactly once by `apply_command_result`. Cloning panics loudly.
    OpenMenu(Box<dyn crate::menu::MenuWidget>),
}

impl std::fmt::Debug for CommandResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lines(v) => f.debug_tuple("Lines").field(v).finish(),
            Self::StyledLines(v) => f.debug_tuple("StyledLines").field(v).finish(),
            Self::Handled => write!(f, "Handled"),
            Self::Error(s) => f.debug_tuple("Error").field(s).finish(),
            Self::ConfigDisplay(v) => f.debug_tuple("ConfigDisplay").field(v).finish(),
            Self::OpenMenu(_) => write!(f, "OpenMenu(<dyn MenuWidget>)"),
        }
    }
}

impl Clone for CommandResult {
    fn clone(&self) -> Self {
        match self {
            Self::Lines(v) => Self::Lines(v.clone()),
            Self::StyledLines(v) => Self::StyledLines(v.clone()),
            Self::Handled => Self::Handled,
            Self::Error(s) => Self::Error(s.clone()),
            Self::ConfigDisplay(v) => Self::ConfigDisplay(v.clone()),
            Self::OpenMenu(_) => {
                panic!("CommandResult::OpenMenu is not Clone — consume it directly")
            }
        }
    }
}

// ── Config display types ──────────────────────────────────────────────────────

/// A named group of config rows for display.
#[derive(Debug, Clone)]
pub struct ConfigSection {
    /// TOML section name, e.g. `"core"` or `"agent.default"`.
    pub name: String,
    /// Rows within this section.
    pub rows: Vec<ConfigRow>,
}

/// One key-value row within a config section.
#[derive(Debug, Clone)]
pub struct ConfigRow {
    pub key: String,
    pub display_value: String,
    pub value_kind: ConfigValueKind,
    pub source_layer: ConfigLayer,
    /// `true` when the value differs from the compiled-in default.
    pub is_overridden: bool,
}

// ── Config types (shared by display + set handler) ────────────────────────────

/// Semantic type of a config value — determines edit behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueKind {
    Str,
    Int,
    Float,
    Bool,
    Array, // read-only in editor
    Table, // read-only in editor
}

/// Which config layer set this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLayer {
    CliFlag,
    EnvVar,
    Session,
    Project,
    User,
    Default,
}

impl ConfigLayer {
    pub fn label(&self) -> &'static str {
        match self {
            Self::CliFlag => "cli",
            Self::EnvVar => "env",
            Self::Session => "session",
            Self::Project => "project",
            Self::User => "user",
            Self::Default => "default",
        }
    }
}

// ── AgentStats ────────────────────────────────────────────────────────────────

/// Per-agent LLM usage statistics.
#[derive(Debug, Clone, Default)]
pub struct AgentStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
    pub calls: u32,
}

// ── AgentStatsMap ─────────────────────────────────────────────────────────────

/// Ordered map of agent name → stats. Preserves insertion order.
#[derive(Debug, Clone, Default)]
pub struct AgentStatsMap {
    entries: Vec<(String, AgentStats)>,
}

impl AgentStatsMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: String, stats: AgentStats) {
        if let Some(e) = self.entries.iter_mut().find(|(k, _)| k == &name) {
            e.1 = stats;
        } else {
            self.entries.push((name, stats));
        }
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut AgentStats> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &AgentStats)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

// ── CommandContext ────────────────────────────────────────────────────────────

/// Everything a handler may need at execution time.
pub struct CommandContext {
    /// Raw args string (everything after the trigger word, trimmed).
    pub args: String,
    /// Parsed `SlashCommand`.
    pub command: SlashCommand,
    /// Application-wide signal bus.
    pub signals: Arc<SignalBus>,
    /// Current resolved config (read-only snapshot).
    pub config: Arc<XaftConfig>,
    /// Active session ID, if any.
    pub session_id: Option<String>,
    /// Working directory for the current session.
    pub working_dir: PathBuf,
    /// Terminal width in columns.
    pub terminal_cols: u16,
    /// Accumulated per-agent LLM stats.
    pub llm_stats: Arc<RwLock<AgentStatsMap>>,
    /// Access to the conversation store.
    pub conversation_store: Option<Arc<dyn ConversationStore>>,
    /// Access to the session store.
    pub session_store: Option<Arc<dyn SessionStore>>,
}

// ── SlashCommandExecuted signal ───────────────────────────────────────────────

/// Emitted on the SignalBus every time a slash command runs.
#[derive(Debug, Clone)]
pub struct SlashCommandExecuted {
    pub name: String,
    pub args: String,
    pub success: bool,
    pub duration_ms: f64,
}
