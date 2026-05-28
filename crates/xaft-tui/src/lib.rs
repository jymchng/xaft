//! `xaft-tui` — ratatui terminal UI for xaft.
//!
//! Provides a 60fps TUI with:
//! - **Conversation pane** — agent text output, system messages, error display
//! - **Tool log sidebar** — live tool activity with duration/status indicators
//! - **Status bar** — current phase, token usage, cost, git branch
//! - **Approval modal** — blocking dialog for tool confirmation (Y/N)
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
//!   ├── 60fps tick → TuiEvent::Tick
//!   └── Main loop:
//!         drain TuiEvent → AppState::handle_event()
//!         render_frame(AppState)
//! ```

#![warn(missing_docs)]

pub mod agent_tracker;
pub mod app;
pub mod approval;
pub mod approval_gate;
pub mod bridge;
pub mod error;
pub mod layout;
pub mod renderer;
pub mod state;
pub mod surface;
pub mod theme;
pub mod widgets;

pub use agent_tracker::{AgentNode, AgentStatus, AgentTracker, ToolCallInfo};
pub use app::TuiApp;
pub use approval::{
    ApprovalContext, ApprovalDecision, ApprovalQueue, AutoApproveConfig, RiskLevel, ToolPreview,
    is_new_file,
};
pub use approval_gate::{AutoApproveGate, TuiApprovalGate};
pub use bridge::{EventBridge, TuiEvent};
pub use error::TuiError;
pub use layout::{
    FoldState, KeyHandled, LayoutManager, LayoutNode, LayoutPreset, LayoutSolution, NavDirection,
    PANE_MINIMA, PaneContent, PaneId, PanePriority, PaneType, PersistedPaneState, SplitDirection,
    pane_type_min_size, solve_for_terminal_size, solve_layout,
};
pub use renderer::{TokenStreamRenderer, display_width, word_wrap};
pub use state::{AppState, FocusedPanel, OutputKind, ToolEntryState, WorkflowPhase};
pub use theme::Theme;
pub use widgets::diff::{DiffMode, DiffViewerState, ParsedFileDiff, ParsedHunk};
pub use widgets::file_tree::FileTreeWidget;
