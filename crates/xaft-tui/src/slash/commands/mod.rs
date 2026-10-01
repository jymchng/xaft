//! Slash command handler implementations.

pub mod agents;
pub mod cancel;
pub mod clear;
pub mod commands_list;
pub mod commit;
pub mod compact;
pub mod config;
pub mod cost;
pub mod diff;
pub mod doctor;
pub mod expand;
pub mod help;
pub mod history;
pub mod init;
pub mod login;
pub mod mcp;
pub mod memory;
pub mod mode_cmd;
pub mod model;
pub mod models;
pub mod permissions;
pub mod pr;
pub mod replay;
pub mod resume;
pub mod rewind;
pub mod status;
pub mod theme;
pub mod tools;
pub mod vim;
pub mod workflow_cmd;

use std::sync::{Arc, RwLock};

use xaft_config::XaftConfig;

#[allow(unused_imports)]
use super::parser::COMMAND_TABLE;
use super::registry::{SlashCommandRegistry, SlashHandler};
use super::{AgentStatsMap, SlashCommand};

pub use agents::AgentsHandler;
pub use cancel::CancelHandler;
pub use clear::ClearHandler;
pub use commands_list::CommandsHandler;
pub use commit::CommitHandler;
pub use compact::CompactHandler;
pub use config::ConfigHandler;
pub use cost::CostHandler;
pub use diff::DiffHandler;
pub use doctor::DoctorHandler;
pub use expand::ExpandHandler;
pub use help::HelpHandler;
pub use history::HistoryHandler;
pub use init::InitHandler;
pub use login::{LoginHandler, LogoutHandler};
pub use mcp::McpHandler;
pub use memory::MemoryHandler;
pub use model::ModelHandler;
pub use models::ModelsHandler;
pub use permissions::PermissionsHandler;
pub use pr::PrHandler;
pub use replay::ReplayHandler;
pub use resume::ResumeHandler;
pub use rewind::RewindHandler;
pub use status::StatusHandler;
pub use theme::ThemeHandler;
pub use tools::ToolsHandler;
pub use vim::{EmacsHandler, VimHandler};
pub use workflow_cmd::{WorkflowHandler, WorkflowSelection, WorkflowsHandler};

// ── Quit handler (no-op, handled specially) ────────────────────────────────

/// No-op handler for /quit — the AppState handles quit specially.
pub struct QuitHandler;

impl SlashHandler for QuitHandler {
    fn description(&self) -> &'static str {
        "Exit xaft"
    }

    fn execute(&self, _ctx: super::CommandContext) -> super::CommandResult {
        // Should never be called — AppState handles Quit before registry lookup.
        super::CommandResult::Lines(vec!["Quitting…".into()])
    }
}

// ── Default registry builder ──────────────────────────────────────────────────

/// Build the default registry with all handlers registered.
/// Handlers that need runtime connections use stub implementations.
///
/// The `/workflow` handler this creates holds a selection no run loop can
/// reach, so `/workflow` will confirm and change nothing here. Anything that
/// also runs tasks must use [`build_registry_with_session`] with the same
/// `Arc` it hands to [`crate::state::AppState::workflow_selection`].
pub fn build_default_registry() -> SlashCommandRegistry {
    build_registry_with_config(
        Arc::new(XaftConfig::default()),
        Arc::new(RwLock::new(AgentStatsMap::new())),
    )
}

/// Build a registry with a specific config and stats map (for testing).
///
/// Same caveat as [`build_default_registry`]: the workflow selection is
/// created here and never shared, so `/workflow` is inert. Fine for parsing
/// and dispatch tests, wrong for a live session.
pub fn build_registry_with_config(
    config: Arc<XaftConfig>,
    agent_stats: Arc<RwLock<AgentStatsMap>>,
) -> SlashCommandRegistry {
    build_registry_with_session(config, agent_stats, Arc::new(WorkflowSelection::new()))
}

/// Build a registry wired to a live session.
///
/// `workflow_selection` is the session-local override `/workflow` writes and
/// the run loop reads. It is a parameter rather than an internally-created
/// value because a selection the run loop cannot see is a command that prints
/// a confirmation and changes nothing — the same false-parity shape the
/// tool and slash reachability guards exist to catch.
pub fn build_registry_with_session(
    config: Arc<XaftConfig>,
    agent_stats: Arc<RwLock<AgentStatsMap>>,
    workflow_selection: Arc<WorkflowSelection>,
) -> SlashCommandRegistry {
    // ThemeHandler needs Arc<RwLock<XaftConfig>> so it can mutate the theme.
    let config_rw = Arc::new(RwLock::new((*config).clone()));

    SlashCommandRegistry::builder()
        .register(SlashCommand::Help, HelpHandler::new_with_table())
        .register(SlashCommand::Clear, ClearHandler)
        .register(SlashCommand::Compact, CompactHandler)
        .register(
            SlashCommand::Config,
            ConfigHandler::new(Arc::clone(&config)),
        )
        .register(SlashCommand::Cost, CostHandler)
        .register(SlashCommand::Init, InitHandler)
        .register(SlashCommand::Agents, AgentsHandler)
        .register(SlashCommand::Mcp, McpHandler)
        .register(SlashCommand::Resume { id: None }, ResumeHandler)
        .register(SlashCommand::Rewind { msg_index: None }, RewindHandler)
        .register(
            SlashCommand::Permissions,
            PermissionsHandler::new(Arc::clone(&config)),
        )
        .register(
            SlashCommand::Model {
                name: String::new(),
            },
            ModelHandler::new(),
        )
        .register(SlashCommand::Vim, VimHandler)
        .register(SlashCommand::Emacs, EmacsHandler)
        .register(
            SlashCommand::Theme { name: None },
            ThemeHandler::new(config_rw),
        )
        .register(SlashCommand::Login, LoginHandler)
        .register(SlashCommand::Logout, LogoutHandler)
        .register(SlashCommand::Doctor, DoctorHandler)
        .register(SlashCommand::Memory, MemoryHandler)
        .register(SlashCommand::Diff, DiffHandler)
        .register(SlashCommand::Commit, CommitHandler)
        .register(SlashCommand::Pr { title: None }, PrHandler)
        .register(SlashCommand::Quit, QuitHandler)
        .register(
            SlashCommand::Mode { name: None },
            mode_cmd::ModeHandler::new(Arc::clone(&config)),
        )
        .register(SlashCommand::Cancel, CancelHandler)
        .register(SlashCommand::Commands, CommandsHandler)
        .register(SlashCommand::Tools, ToolsHandler)
        .register(SlashCommand::Status, StatusHandler)
        .register(SlashCommand::Expand { target: None }, ExpandHandler)
        .register(SlashCommand::History, HistoryHandler)
        .register(SlashCommand::Models, ModelsHandler)
        .register(SlashCommand::Workflows, WorkflowsHandler)
        .register(
            SlashCommand::Workflow { name: None },
            WorkflowHandler::new(workflow_selection),
        )
        .register(SlashCommand::Replay { id: None }, ReplayHandler)
        .build()
}
