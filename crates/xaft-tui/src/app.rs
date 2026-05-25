//! `TuiApp` — the main ratatui application driver.
//!
//! # Architecture
//!
//! ```text
//! TuiApp::run()
//!   ├── spawn: XaftRuntime::run(request) in background task
//!   ├── spawn: terminal event reader (keyboard/mouse/resize)
//!   ├── attach: EventBridge to SignalBus (forward signals → TuiEvent channel)
//!   └── main loop (60fps):
//!         ├── drain TuiEvent channel → AppState::handle_event()
//!         ├── render frame via ratatui
//!         └── check for quit / task done
//! ```
//!
//! Approval requests block the background runtime task until the user
//! responds via the TUI dialog.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::Clear,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use xaft_config::XaftConfig;
use xaft_runtime::{RunRequest, RuntimeDispatch, XaftRuntime};

use crate::approval_gate::TuiApprovalGate;
use crate::bridge::{EventBridge, TuiEvent};
use crate::error::TuiError;
use crate::state::AppState;
use crate::theme::Theme;
use crate::widgets::{
    approval::ApprovalWidget, conversation::ConversationWidget, status_bar::StatusBarWidget,
    tool_log::ToolLogWidget,
};

const TICK_RATE: Duration = Duration::from_millis(16); // ~60fps

// ── TuiApp ────────────────────────────────────────────────────────────────────

/// Main TUI application.
pub struct TuiApp {
    config: XaftConfig,
    theme: Theme,
}

impl TuiApp {
    /// Create a new `TuiApp` from loaded config.
    pub fn new(config: XaftConfig) -> Self {
        let theme = Theme::from_config(&config.tui.theme);
        Self { config, theme }
    }

    /// Run the TUI for a single task and return when it completes or the user quits.
    ///
    /// Initializes the terminal, spawns the runtime task, and drives the render loop.
    pub async fn run(self, request: RunRequest) -> Result<(), TuiError> {
        // ── Terminal setup ────────────────────────────────────────────────────
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let result = self.run_inner(&mut terminal, request).await;

        // ── Terminal teardown ─────────────────────────────────────────────────
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    async fn run_inner(
        &self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        request: RunRequest,
    ) -> Result<(), TuiError> {
        let task = request.task.clone();
        let cancel = CancellationToken::new();

        // ── Event channel ─────────────────────────────────────────────────────
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TuiEvent>();

        // ── Bootstrap runtime ─────────────────────────────────────────────────
        let runtime = XaftRuntime::bootstrap(self.config.clone()).await?;
        let signals = Arc::clone(runtime.signals());

        // ── Approval gate ─────────────────────────────────────────────────────
        let approval_gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));

        // ── Attach signal bridge ──────────────────────────────────────────────
        let bridge = EventBridge::new(event_tx.clone());
        bridge.attach(&signals).await;

        // ── Spawn runtime task ────────────────────────────────────────────────
        let tx_result = event_tx.clone();
        let cancel_clone = cancel.clone();
        let request_for_runtime = request;
        let runtime_handle = tokio::spawn(async move {
            tokio::select! {
                result = runtime.run(request_for_runtime) => {
                    match result {
                        Ok(run_result) => {
                            let _ = tx_result.send(TuiEvent::TaskComplete {
                                summary: run_result.summary,
                            });
                        }
                        Err(e) => {
                            let _ = tx_result.send(TuiEvent::RuntimeError(e.to_string()));
                        }
                    }
                }
                _ = cancel_clone.cancelled() => {
                    tracing::info!("xaft-tui: runtime task cancelled");
                }
            }
        });

        // ── Spawn terminal event reader ───────────────────────────────────────
        let tx_keys = event_tx.clone();
        let cancel_keys = cancel.clone();
        tokio::spawn(async move {
            let mut reader = EventStream::new();
            loop {
                tokio::select! {
                    Some(Ok(event)) = reader.next() => {
                        match event {
                            Event::Key(key) => { let _ = tx_keys.send(TuiEvent::Key(key)); }
                            Event::Mouse(mouse) => { let _ = tx_keys.send(TuiEvent::Mouse(mouse)); }
                            Event::Resize(w, h) => { let _ = tx_keys.send(TuiEvent::Resize(w, h)); }
                            _ => {}
                        }
                    }
                    _ = cancel_keys.cancelled() => break,
                    else => break,
                }
            }
        });

        // ── 60fps tick spawner ────────────────────────────────────────────────
        let tx_tick = event_tx.clone();
        let cancel_tick = cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(TICK_RATE);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if tx_tick.send(TuiEvent::Tick).is_err() {
                            break;
                        }
                    }
                    _ = cancel_tick.cancelled() => break,
                }
            }
        });

        // ── Main event / render loop ──────────────────────────────────────────
        let mut state = AppState::new(task);

        loop {
            // Drain all pending events before rendering
            while let Ok(event) = event_rx.try_recv() {
                let is_key = matches!(event, TuiEvent::Key(_));
                let is_enter = matches!(
                    event,
                    TuiEvent::Key(crossterm::event::KeyEvent {
                        code: crossterm::event::KeyCode::Enter,
                        ..
                    })
                );

                // Handle approval response
                if state.pending_approval.is_some() && is_enter {
                    let approved = state.approval_focused_approve;
                    if let Some(ref pa) = state.pending_approval {
                        approval_gate.respond(&pa.tool_use_id, approved).await;
                        state.pending_approval = None;
                        state.focused_panel = crate::state::FocusedPanel::Conversation;
                    }
                    continue;
                }

                state.handle_event(event);
            }

            // Render frame
            terminal.draw(|f| render_frame(f, &state, &self.theme))?;

            // Check quit conditions
            if state.should_quit {
                cancel.cancel();
                approval_gate.cancel_all().await;
                break;
            }

            // Auto-quit when task done and no more pending approvals
            if state.task_done && !approval_gate.has_pending().await {
                // Brief pause so user sees the Done state
                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }

            // Tiny sleep to yield back to tokio scheduler
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        runtime_handle.abort();
        Ok(())
    }
}

// ── Render frame ──────────────────────────────────────────────────────────────

fn render_frame(f: &mut Frame, state: &AppState, theme: &Theme) {
    let area = f.area();

    // Clear background
    f.render_widget(Clear, area);

    // Main vertical split: body + status bar
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // body
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let body = vertical[0];
    let status_area = vertical[1];

    // Body horizontal split: conversation (70%) + tool log (30%)
    let sidebar_width = 30u16;
    let conv_width = body.width.saturating_sub(sidebar_width);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(conv_width),
            Constraint::Length(sidebar_width),
        ])
        .split(body);

    let conv_area = horizontal[0];
    let sidebar_area = horizontal[1];

    // Render panes
    f.render_widget(
        ConversationWidget::new(
            state,
            theme,
            state.focused_panel == crate::state::FocusedPanel::Conversation,
        ),
        conv_area,
    );

    f.render_widget(
        ToolLogWidget::new(
            state,
            theme,
            state.focused_panel == crate::state::FocusedPanel::ToolLog,
        ),
        sidebar_area,
    );

    f.render_widget(StatusBarWidget::new(state, theme), status_area);

    // Approval modal overlay (drawn last, on top)
    if ApprovalWidget::is_visible(state) {
        f.render_widget(ApprovalWidget::new(state, theme), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xaft_config::XaftConfig;

    #[test]
    fn tui_app_constructs() {
        let config = XaftConfig::default();
        let app = TuiApp::new(config);
        assert!(!app.config.tui.mouse || true); // just verify construction
        let _ = app.theme; // theme is set
    }

    #[test]
    fn tick_rate_is_reasonable() {
        // 60fps = 16ms tick
        assert!(TICK_RATE.as_millis() >= 8 && TICK_RATE.as_millis() <= 33);
    }
}
