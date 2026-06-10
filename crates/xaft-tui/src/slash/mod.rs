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
    Resume { id: Option<String> },
    Rewind { msg_index: Option<usize> },
    Permissions,
    Model { name: String },
    Vim,
    Emacs,
    Theme { name: Option<String> },
    Login,
    Logout,
    Doctor,
    Memory,
    Diff,
    Commit,
    Pr { title: Option<String> },
    Quit,
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
        }
    }
}

// ── CommandResult ─────────────────────────────────────────────────────────────

/// The result of executing a slash command.
#[derive(Debug, Clone)]
pub enum CommandResult {
    /// Zero or more plaintext lines to print to the transcript.
    Lines(Vec<String>),
    /// Lines with explicit styling (for tables, errors, etc.).
    StyledLines(Vec<StyledLine>),
    /// The command pushed mutations directly; nothing more to do.
    Handled,
    /// The command failed; print an error line.
    Error(String),
    /// Interactive config editor (Feature B) — opens a navigable panel.
    ConfigEditor(Vec<ConfigEntry>),
}

// ── Config editor types (Feature B) ──────────────────────────────────────────

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

/// One flattened config entry for the config editor.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub section: String,
    pub key: String,
    pub display_value: String,
    pub raw_value: String,
    pub value_kind: ConfigValueKind,
    pub source_layer: ConfigLayer,
    pub editable: bool,
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
