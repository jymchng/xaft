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

use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
        let use_alt = self.config.tui.use_alternate_screen;
        let preserve = self.config.tui.preserve_output_on_exit;
        let persist_frame = self.config.tui.persist_final_frame;
        let mouse = self.config.tui.mouse;

        // ── Signal handler ───────────────────────────────────────────────────
        // Set a flag on SIGINT so the event loop can exit gracefully even if
        // the signal arrives outside of crossterm event reading.
        let sigint_received = Arc::new(AtomicBool::new(false));
        {
            let flag = Arc::clone(&sigint_received);
            let _ = ctrlc::set_handler(move || {
                flag.store(true, Ordering::SeqCst);
            });
        }

        // ── Terminal setup ────────────────────────────────────────────────────
        tracing::info!(
            use_alt,
            preserve,
            persist_frame,
            mouse,
            "xaft: terminal init"
        );
        enable_raw_mode()?;
        let mut stdout = io::stdout();

        if use_alt {
            if mouse {
                execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
            } else {
                execute!(stdout, EnterAlternateScreen)?;
            }
        } else if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        if use_alt {
            terminal.clear()?;
        }

        let result = self
            .run_inner(&mut terminal, request, Arc::clone(&sigint_received))
            .await;

        // ── Terminal teardown ─────────────────────────────────────────────────
        // Capture final frame buffer for replay after leaving alternate screen.
        let final_frame = if preserve && use_alt && persist_frame {
            terminal.draw(|_| {})?; // flush pending changes
            Some(terminal.current_buffer_mut().clone())
        } else {
            None
        };

        tracing::info!("xaft: terminal teardown");
        disable_raw_mode()?;

        if use_alt {
            if mouse {
                execute!(
                    terminal.backend_mut(),
                    LeaveAlternateScreen,
                    DisableMouseCapture
                )?;
            } else {
                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            }
        } else if mouse {
            execute!(terminal.backend_mut(), DisableMouseCapture)?;
        }

        terminal.show_cursor()?;

        // ── Replay final frame to stdout (preserves content in scrollback) ──
        if let Some(buf) = final_frame {
            tracing::info!("xaft: replaying final frame to stdout");
            let mut out = io::stdout();
            let area = buf.area;
            for y in 0..area.height {
                let mut line = String::new();
                for x in 0..area.width {
                    let cell = &buf[(x, y)];
                    line.push_str(cell.symbol());
                }
                // Trim trailing whitespace
                let trimmed = line.trim_end();
                let _ = writeln!(out, "{}", trimmed);
            }
            let _ = out.flush();
        }

        result
    }

    async fn run_inner(
        &self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        request: RunRequest,
        sigint_received: Arc<AtomicBool>,
    ) -> Result<(), TuiError> {
        let task = request.task.clone();
        let cancel = CancellationToken::new();

        // Save fields before request is moved
        let working_dir = request.working_dir.clone();
        let dangerously_skip_permissions = request.dangerously_skip_permissions;

        // ── Event channel ─────────────────────────────────────────────────────
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TuiEvent>();

        // ── Bootstrap ONE persistent runtime for the entire TUI session ───────
        //
        // This single runtime handles all tasks (initial + subsequent) via a
        // task channel.  One SignalBus + one EventBridge = no race conditions.
        let runtime = XaftRuntime::bootstrap(self.config.clone()).await?;
        let signals = Arc::clone(runtime.signals());

        // ── Approval gate — normal TUI gate or auto-approve ──────────────────
        //
        // When `--dangerously-skip-permissions` was passed, show a blocking
        // danger modal before the run proceeds.  If the user confirms, replace
        // the gate with an `AutoApproveGate` that approves every tool call.
        // If they reject, abort immediately.
        let approval_gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));

        let effective_gate: Arc<dyn agtrs_runtime::approval::ApprovalGate> =
            if dangerously_skip_permissions {
                // Show danger confirmation in the terminal before TUI takes over.
                let confirmed = show_danger_confirmation_terminal();
                if !confirmed {
                    return Err(TuiError::Approval(
                        "user aborted --dangerously-skip-permissions".into(),
                    ));
                }
                Arc::new(crate::approval_gate::AutoApproveGate)
                    as Arc<dyn agtrs_runtime::approval::ApprovalGate>
            } else {
                Arc::clone(&approval_gate) as Arc<dyn agtrs_runtime::approval::ApprovalGate>
            };

        let runtime = runtime.with_approval_gate(effective_gate);

        // ── Single bridge — attached once, reused for all tasks ───────────────
        let bridge = EventBridge::new(event_tx.clone());
        bridge.attach(&signals).await;

        // ── Persistent task channel ───────────────────────────────────────────
        //
        // The TUI sends `RunRequest`s here; the runtime loop processes them
        // sequentially so agent state never overlaps between tasks.
        let (task_tx, mut task_rx) = mpsc::unbounded_channel::<RunRequest>();

        // Enqueue the initial task immediately if one was provided.
        if !task.is_empty() {
            let _ = task_tx.send(request);
        }

        // ── Spawn persistent runtime loop ─────────────────────────────────────
        let tx_result = event_tx.clone();
        let cancel_runtime = cancel.clone();
        let runtime_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    maybe_req = task_rx.recv() => {
                        match maybe_req {
                            Some(req) => {
                                match runtime.run(req).await {
                                    Ok(r) => {
                                        let _ = tx_result.send(TuiEvent::TaskComplete {
                                            summary: r.summary,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx_result
                                            .send(TuiEvent::RuntimeError(e.to_string()));
                                    }
                                }
                            }
                            // Channel closed — TUI is shutting down
                            None => break,
                        }
                    }
                    _ = cancel_runtime.cancelled() => {
                        tracing::info!("xaft-tui: runtime loop cancelled");
                        break;
                    }
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
        // Channel for user-typed tasks from the InputBar.
        let (user_msg_tx, mut user_msg_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let mut state = AppState::new(task.clone());
        state.user_message_tx = Some(user_msg_tx);

        // When started with no task, focus the InputBar immediately
        if task.is_empty() {
            state.layout_manager.focus_type(PaneType::InputBar);
            state.sync_focused_panel();
        }

        // `agent_running`: true while an agent task is in flight.
        let mut agent_running = !task.is_empty();

        loop {
            // Accept a new task when idle or when the previous task finished.
            let can_accept = !agent_running || state.task_done;
            if can_accept {
                if let Ok(user_task) = user_msg_rx.try_recv() {
                    if state.task_done {
                        state.push_separator();
                    }
                    state.task = user_task.clone();
                    state.task_done = false;
                    state.phase = crate::state::WorkflowPhase::Planning;
                    state.reset_for_new_task();
                    state.layout_manager.focus_type(PaneType::Chat);
                    state.sync_focused_panel();

                    // Send to the persistent runtime loop — NO new bootstrap.
                    // The same SignalBus and EventBridge handle all tasks.
                    let _ = task_tx.send(RunRequest {
                        task: user_task,
                        config: self.config.clone(),
                        working_dir: working_dir.clone(),
                        headless: false,
                        dry_run: false,
                        auto_approve: false,
                        dangerously_skip_permissions,
                        resume_session_id: None,
                        workflow: xaft_runtime::WorkflowConfig::default(),
                    });
                    agent_running = true;
                }
            }

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

            // Check quit conditions (keyboard or SIGINT)
            if state.should_quit || sigint_received.load(Ordering::SeqCst) {
                tracing::info!("xaft: quit signal received");
                cancel.cancel();
                approval_gate.cancel_all().await;
                break;
            }

            // When task is done: mark agent idle + focus InputBar for next task.
            if state.task_done && agent_running && !approval_gate.has_pending().await {
                agent_running = false;
                if state.layout_manager.focused_type() != Some(PaneType::InputBar) {
                    state.layout_manager.focus_type(PaneType::InputBar);
                    state.sync_focused_panel();
                }
            }

            // Tiny sleep to yield back to tokio scheduler
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        runtime_handle.abort();
        Ok(())
    }
}

// ── Danger confirmation ───────────────────────────────────────────────────────

/// Show a full-screen danger confirmation in the raw terminal (before the TUI
/// alternate screen starts).  Returns `true` if the user typed `yes`.
fn show_danger_confirmation_terminal() -> bool {
    use std::io::{BufRead, Write};

    let warning = "\
\x1b[1;31m╔══════════════════════════════════════════════════════════════╗\x1b[0m
\x1b[1;31m║         ⚠  DANGEROUS MODE — SKIP ALL PERMISSIONS  ⚠         ║\x1b[0m
\x1b[1;31m╚══════════════════════════════════════════════════════════════╝\x1b[0m

  \x1b[1m--dangerously-skip-permissions\x1b[0m disables ALL approval gates.

  Agents will execute commands WITHOUT asking for confirmation:
    • Shell commands (rm -rf, chmod, etc.)
    • File deletion and overwrite
    • Network requests
    • Any other tool call

  This is intended for trusted environments only (e.g. isolated
  containers, CI pipelines where you own the workspace).

  \x1b[33mType 'yes' to proceed  or  press Enter to abort:\x1b[0m ";

    eprint!("{warning}");
    let _ = std::io::stderr().flush();

    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return false;
    }
    line.trim().eq_ignore_ascii_case("yes")
}

// ── Render frame ──────────────────────────────────────────────────────────────

fn render_frame(f: &mut Frame, state: &AppState, theme: &Theme) {
    let area = f.area();

    // Clear background
    f.render_widget(Clear, area);

    // Solve the layout from the manager
    let solution = state.layout_manager.solve(area);

    // Borderless design — no split borders drawn.
    // Visual separation comes from per-pane background colors
    // (chat=bg, inputbar=statusbar_bg, usagebar=statusbar_bg, statusbar=statusbar_bg).

    // Chat pane
    if let Some(rect) = solution.rect_for_type(PaneType::Chat) {
        let focused = state.layout_manager.focused_type() == Some(PaneType::Chat);
        f.render_widget(ConversationWidget::new(state, theme, focused), rect);
    }

    // Input bar pane
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
