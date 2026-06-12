//! `xaft-tui` — conversational streaming terminal renderer for xaft.
//!
//! Renders agent output, tool calls, and streaming tokens as an append-only
//! transcript in the primary terminal buffer. History scrolls naturally into
//! terminal scrollback. No Ratatui frame-buffer or alternate screen.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use xaft_tui::TuiApp;
//! use xaft_config::XaftConfig;
//! use xaft_runtime::{RunRequest, XaftRuntime};
//! use std::path::PathBuf;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = XaftConfig::default();
//! let app = TuiApp::new(config.clone());
//!
//! let request = RunRequest {
//!     task: "Fix the type error in src/".into(),
//!     config,
//!     working_dir: PathBuf::from("."),
//!     headless: false,
//!     dry_run: false,
//!     auto_approve: false,
//!     dangerously_skip_permissions: false,
//!     resume_session_id: None,
//!     workflow: xaft_runtime::WorkflowConfig::default(),
//!     prior_messages: vec![],
//!     user_message: None,
//! };
//!
//! app.run(request).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Architecture
//!
//! ```text
//! TuiApp::run()
//!   ├── XaftRuntime spawned in background task
//!   ├── EventBridge subscribes to SignalBus → TuiEvent channel
//!   ├── Terminal key/mouse events → TuiEvent channel
//!   └── Main loop:
//!         drain TuiEvent → AppState::handle_event() → RenderMutation
//!         apply RenderMutation → IncrementalRenderer (stdout)
//!         ephemeral refresh at 10fps
//! ```

pub mod agent_tracker;
pub mod app;
pub mod approval;
pub mod approval_gate;
pub mod bridge;
pub mod confirm;
pub mod ephemeral;
pub mod error;
pub mod input_bar;
pub mod mention;
pub mod mention_signals;
pub mod prompt;
pub mod render;
pub mod renderer;
pub mod slash;
pub mod state;
pub mod surface;
pub mod theme;
pub mod transcript;
pub mod user_message;

pub use agent_tracker::{AgentNode, AgentStatus, AgentTracker, ToolCallInfo};
pub use app::TuiApp;
pub use approval::{
    ApprovalContext, ApprovalDecision, ApprovalQueue, AutoApproveConfig, RiskLevel, ToolPreview,
    is_new_file,
};
pub use approval_gate::{AutoApproveGate, TuiApprovalGate};
pub use bridge::{EventBridge, TuiEvent};
pub use confirm::{EscapeConfirmDialog, EscapeConfirmOutcome, escape_reason_str};
pub use ephemeral::{EphemeralState, build_ephemeral};
pub use error::TuiError;
pub use input_bar::{Cursor, InputAction, InputBar, MAX_VISIBLE_ROWS, PREFIX_WIDTH, wrap_rows};
pub use mention::{ExpandedMessage, MentionError, MentionResolver, MentionToken, ResolvedFile};
pub use mention_signals::{
    emit_escape_approved, emit_escape_denied, escape_signal_entry, file_ref_attached_entry,
    file_ref_blocks, file_ref_not_found_entry, mentions_resolved_entry, path_from_warning,
};
// Re-export the F3 escape types from agtrs_runtime::transport for the
// `xaft-tui` API surface; the mention resolver records them in
// `ResolvedFile::escape` and `ExpandedMessage::escape_mentions`.
pub use agtrs_runtime::transport::{EscapeInfo, EscapeReason};
pub use prompt::{
    PromptState, SlashPaletteRow, SlashPaletteSnapshot, build_prompt, empty_buffer_hint,
    format_prompt_line, scroll_indicator_above,
};
pub use render::MarkdownRenderer;
pub use renderer::{IncrementalRenderer, display_width, style_for_kind, word_wrap};
pub use slash::commands::build_default_registry;
pub use slash::palette::{SlashHistory, SlashPalette};
pub use slash::parser::SlashCommandParser;
pub use slash::registry::{SlashCommandRegistry, SlashHandler};
pub use slash::{
    AgentStats, AgentStatsMap, CommandContext, CommandResult, ConfigLayer, ConfigRow,
    ConfigSection, ConfigValueKind, SlashCommand, SlashCommandExecuted,
};
pub use state::{
    AppState, WorkflowPhase, commit_line_texts, format_elapsed, format_tokens_compact,
};
pub use surface::{ConversationalSurface, render_exit_summary};
pub use theme::Theme;
pub use transcript::{
    LineKind, LineStyle, RenderMutation, SpanColor, StyledLine, StyledSpan, build_file_diff_lines,
};
pub use user_message::UserMessage;
