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
//!     resume_session_id: None,
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

pub mod app;
pub mod approval_gate;
pub mod bridge;
pub mod error;
pub mod layout;
pub mod state;
pub mod theme;
pub mod widgets;

pub use app::TuiApp;
pub use approval_gate::TuiApprovalGate;
pub use bridge::{EventBridge, TuiEvent};
pub use error::TuiError;
pub use layout::{LayoutManager, LayoutNode, PaneId, PanePriority, PaneType, SplitDirection};
pub use state::{AppState, FocusedPanel, OutputKind, ToolEntryState, WorkflowPhase};
pub use theme::Theme;
pub use widgets::diff::{DiffMode, DiffViewerState, ParsedFileDiff, ParsedHunk};
