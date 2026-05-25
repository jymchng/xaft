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
use ratatui::{Frame, Terminal, backend::CrosstermBackend, widgets::Clear};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use xaft_config::XaftConfig;
use xaft_runtime::{RunRequest, RuntimeDispatch, XaftRuntime};

use crate::approval_gate::TuiApprovalGate;
use crate::bridge::{EventBridge, TuiEvent};
use crate::error::TuiError;
use crate::layout::PaneType;
use crate::state::AppState;
use crate::theme::Theme;
use crate::widgets::{
    agent_activity::AgentActivityWidget, approval::ApprovalWidget,
    conversation::ConversationWidget, diff::DiffWidget, file_tree::FileTreeWidget,
    input_bar::InputBarWidget, status_bar::StatusBarWidget, token_dashboard::TokenDashboardWidget,
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
        // Mouse capture blocks OS text selection. Only enable if config requests it.
        if self.config.tui.mouse {
            execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        } else {
            execute!(stdout, EnterAlternateScreen)?;
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let result = self.run_inner(&mut terminal, request).await;

        // ── Terminal teardown ─────────────────────────────────────────────────
        disable_raw_mode()?;
        if self.config.tui.mouse {
            execute!(
                terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
        } else {
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        }
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
                state.handle_event(event);

                // Drain gate decisions queued by keyboard handlers
                for (tool_use_id, approved) in state.pending_gate_decisions.drain(..) {
                    approval_gate.respond(&tool_use_id, approved).await;
                }
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

    // Solve the layout from the manager
    let solution = state.layout_manager.solve(area);

    // Chat pane
    if let Some(rect) = solution.rect_for_type(PaneType::Chat) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::Chat);
        f.render_widget(ConversationWidget::new(state, theme, focused), rect);
    }

    // Input bar pane (task display below chat)
    if let Some(rect) = solution.rect_for_type(PaneType::InputBar) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::InputBar);
        f.render_widget(InputBarWidget::new(state, theme, focused), rect);
    }

    // Agent activity pane
    if let Some(rect) = solution.rect_for_type(PaneType::AgentActivity) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::AgentActivity);
        f.render_widget(AgentActivityWidget::new(state, theme, focused), rect);
    }

    // Token dashboard pane
    if let Some(rect) = solution.rect_for_type(PaneType::TokenDashboard) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::TokenDashboard);
        f.render_widget(TokenDashboardWidget::new(state, theme, focused), rect);
    }

    // Diff viewer pane (only shown when diffs are available or layout forces it)
    if let Some(rect) = solution.rect_for_type(PaneType::DiffViewer) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::DiffViewer);
        f.render_widget(DiffWidget::new(&state.diff, theme, focused), rect);
    }

    // File tree pane
    if let Some(rect) = solution.rect_for_type(PaneType::FileTree) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::FileTree);
        f.render_widget(FileTreeWidget::new(state, theme, focused), rect);
    }

    // Status bar
    if let Some(rect) = solution.rect_for_type(PaneType::StatusBar) {
        f.render_widget(StatusBarWidget::new(state, theme), rect);
    }

    // Draw split borders between panes
    let buf = f.buffer_mut();
    let borders = crate::layout::collect_split_borders(state.layout_manager.root(), area);
    for border in borders {
        match border.direction {
            crate::layout::SplitDirection::Horizontal => {
                for row in border.start..border.end {
                    buf[(border.pos, row)]
                        .set_symbol("\u{2502}") // │
                        .set_style(ratatui::style::Style::default().fg(theme.border));
                }
            }
            crate::layout::SplitDirection::Vertical => {
                for col in border.start..border.end {
                    buf[(col, border.pos)]
                        .set_symbol("\u{2500}") // ─
                        .set_style(ratatui::style::Style::default().fg(theme.border));
                }
            }
        }
    }

    // Approval overlay (always on top)
    if ApprovalWidget::is_visible(&state.approval_queue) {
        f.render_widget(ApprovalWidget::new(&state.approval_queue, theme), area);
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
