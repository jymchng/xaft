//! `TuiApp` — the conversational streaming terminal application driver.
//!
//! # Architecture
//!
//! ```text
//! TuiApp::run()
//!   ├── spawn: XaftRuntime::run(request) in background task
//!   ├── attach: EventBridge to SignalBus (forward signals → TuiEvent channel)
//!   ├── spawn: terminal event reader (keyboard/mouse/resize)
//!   └── main loop:
//!         ├── drain TuiEvent channel → AppState::handle_event()
//!         ├── apply RenderMutation → IncrementalRenderer
//!         ├── ephemeral refresh at ~10fps
//!         └── check for quit / task done
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use xaft_config::XaftConfig;
use xaft_runtime::{RunRequest, RuntimeDispatch, XaftRuntime};

use crate::approval_gate::TuiApprovalGate;
use crate::bridge::{EventBridge, TuiEvent};
use crate::ephemeral;
use crate::error::TuiError;
use crate::prompt::build_prompt;
use crate::renderer::IncrementalRenderer;
use crate::state::AppState;
use crate::surface::{ConversationalSurface, render_exit_summary};
use crate::theme::Theme;
use crate::transcript::RenderMutation;

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

    /// Run the TUI for a session and return when it completes or the user quits.
    pub async fn run(self, request: RunRequest) -> Result<(), TuiError> {
        let show_summary = self.config.tui.show_exit_summary;
        let mouse = self.config.tui.mouse;

        // ── Signal handler ───────────────────────────────────────────────────
        let sigint_received = Arc::new(AtomicBool::new(false));
        {
            let flag = Arc::clone(&sigint_received);
            let _ = ctrlc::set_handler(move || {
                flag.store(true, Ordering::SeqCst);
            });
        }

        // ── Surface ──────────────────────────────────────────────────────────
        let mut surface = ConversationalSurface::new(mouse)?;
        surface.init()?;

        // ── Renderer ─────────────────────────────────────────────────────────
        let mut renderer = IncrementalRenderer::new()?;

        let result = self
            .run_inner(request, sigint_received, &mut renderer, show_summary)
            .await;

        // Shutdown sequence (renderer also calls disable_raw_mode)
        let _ = renderer.shutdown(&self.theme);
        surface.shutdown()?;

        result
    }

    async fn run_inner(
        &self,
        request: RunRequest,
        sigint_received: Arc<AtomicBool>,
        renderer: &mut IncrementalRenderer,
        show_summary: bool,
    ) -> Result<(), TuiError> {
        let task = request.task.clone();
        // Preserve the original --resume ID so it survives to the user's first
        // typed message when xaft is opened without an initial task string.
        // Without this, `xaft --resume <id>` with no task argument loses the ID
        // when the user types their first message (state.session is still None).
        let mut pending_resume_id = request.resume_session_id.clone();
        let cancel = CancellationToken::new();
        let working_dir = request.working_dir.clone();
        let dangerously_skip_permissions = request.dangerously_skip_permissions;

        // ── Event channel ─────────────────────────────────────────────────────
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TuiEvent>();

        // ── Bootstrap runtime ─────────────────────────────────────────────────
        let mut runtime = XaftRuntime::bootstrap(self.config.clone()).await?;

        // Wire SQLite session + conversation stores if available.
        // This is critical for conversation persistence: without this,
        // the TUI runtime uses FsSessionStore and conversation_store=None,
        // meaning all conversation history is lost between tasks.
        #[cfg(feature = "session")]
        if let Ok(mgr) = xaft_session::SessionManager::new(&self.config.core.data_dir).await {
            runtime = runtime.with_stores(mgr.session_store(), mgr.conversation_store());
            tracing::info!("xaft-tui: SQLite session store active");
        }

        let signals = Arc::clone(runtime.signals());

        let approval_gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));

        let effective_gate: Arc<dyn agtrs_runtime::approval::ApprovalGate> =
            if dangerously_skip_permissions {
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

        // ── Event bridge ──────────────────────────────────────────────────────
        let bridge = EventBridge::new(event_tx.clone());
        bridge.attach(&signals).await;

        // ── Task channel ──────────────────────────────────────────────────────
        let (task_tx, mut task_rx) = mpsc::unbounded_channel::<RunRequest>();
        if !task.is_empty() {
            let _ = task_tx.send(request);
        }

        // ── Spawn runtime loop ────────────────────────────────────────────────
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
                                            session: r.session,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx_result.send(TuiEvent::RuntimeError(e.to_string()));
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                    _ = cancel_runtime.cancelled() => break,
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
                            Event::Paste(s) => { let _ = tx_keys.send(TuiEvent::Paste(s)); }
                            _ => {}
                        }
                    }
                    _ = cancel_keys.cancelled() => break,
                    else => break,
                }
            }
        });

        // ── State + initial prompt ────────────────────────────────────────────
        let (user_msg_tx, mut user_msg_rx) = mpsc::unbounded_channel::<String>();
        let mut state = AppState::new(task.clone());
        state.user_message_tx = Some(user_msg_tx);

        let initial_prompt = build_prompt(&state);
        renderer.init_prompt(&initial_prompt, &self.theme)?;

        let mut agent_running = !task.is_empty();
        let mut last_ephemeral = tokio::time::Instant::now();
        const EPHEMERAL_INTERVAL: Duration = Duration::from_millis(100);

        // ── Main loop ─────────────────────────────────────────────────────────
        loop {
            // Accept new task when idle or previous task is done.
            let can_accept = !agent_running || state.task_done;
            if can_accept {
                if let Ok(user_task) = user_msg_rx.try_recv() {
                    state.task = user_task.clone();
                    state.task_done = false;
                    state.phase = crate::state::WorkflowPhase::Planning;
                    state.reset_for_new_task();

                    // Use the original --resume ID on the first typed task (when the
                    // TUI was opened without an initial task argument). For all
                    // subsequent tasks, use the session that completed most recently.
                    let resume_id = pending_resume_id
                        .take()
                        .or_else(|| state.session.as_ref().map(|s| s.id.to_string()));

                    let _ = task_tx.send(RunRequest {
                        task: user_task,
                        config: self.config.clone(),
                        working_dir: working_dir.clone(),
                        headless: false,
                        dry_run: false,
                        auto_approve: false,
                        dangerously_skip_permissions,
                        resume_session_id: resume_id,
                        workflow: xaft_runtime::WorkflowConfig::default(),
                        prior_messages: vec![],
                    });
                    agent_running = true;
                    if state.session_start_time.is_none() {
                        state.session_start_time = Some(std::time::Instant::now());
                    }
                }
            }

            // Drain all pending events.
            while let Ok(event) = event_rx.try_recv() {
                state.handle_event(event);
                for (tool_use_id, approved) in state.pending_gate_decisions.drain(..) {
                    approval_gate.respond(&tool_use_id, approved).await;
                }
            }

            // Apply render mutations.
            for mutation in state.mutations.drain(..) {
                apply_mutation(renderer, mutation, &self.theme)?;
            }

            // Ephemeral refresh at ~10fps.
            if last_ephemeral.elapsed() >= EPHEMERAL_INTERVAL {
                state.spinner_tick = state.spinner_tick.wrapping_add(1);
                let eph = ephemeral::build_ephemeral(&state);
                renderer.set_ephemeral_opt(eph.as_ref(), &self.theme)?;
                last_ephemeral = tokio::time::Instant::now();
            }

            // Ctrl+C timeout check.
            state.check_ctrl_c_timeout();

            // Quit check.
            if state.should_quit || sigint_received.load(Ordering::SeqCst) {
                tracing::info!("xaft: quit signal received");
                cancel.cancel();
                approval_gate.cancel_all().await;
                break;
            }

            // Task done: clear ephemeral, focus prompt.
            if state.task_done && agent_running && !approval_gate.has_pending().await {
                agent_running = false;
                renderer.clear_ephemeral(&self.theme)?;
                let prompt = build_prompt(&state);
                renderer.update_prompt(&prompt, &self.theme)?;
            }

            tokio::task::yield_now().await;
        }

        runtime_handle.abort();

        // Clear the bottom block BEFORE printing any post-exit output,
        // so the prompt borders don't appear mixed with the summary.
        renderer.clear_for_exit(&self.theme)?;

        // Exit summary.
        if show_summary {
            if let Some(start) = state.session_start_time {
                render_exit_summary(
                    start.elapsed(),
                    state.total_input_tokens,
                    state.total_output_tokens,
                    state.total_cost_usd,
                    state.session.as_ref().map(|s| s.id.to_string()).as_deref(),
                );
            }
        }

        Ok(())
    }
}

// ── Mutation dispatcher ───────────────────────────────────────────────────────

fn apply_mutation(
    renderer: &mut IncrementalRenderer,
    mutation: RenderMutation,
    theme: &Theme,
) -> Result<(), TuiError> {
    match mutation {
        RenderMutation::CommitLine(line) => renderer.commit_line(&line, theme)?,
        RenderMutation::StreamToken { fragment, .. } => renderer.update_stream(&fragment, theme)?,
        RenderMutation::FlushStream => renderer.flush_stream(theme)?,
        RenderMutation::SetEphemeral(Some(eph)) => renderer.set_ephemeral(&eph, theme)?,
        RenderMutation::SetEphemeral(None) => renderer.clear_ephemeral(theme)?,
        RenderMutation::UpdatePrompt(p) => renderer.update_prompt(&p, theme)?,
        RenderMutation::Resize { cols, rows } => renderer.handle_resize(cols, rows, theme)?,
        RenderMutation::Shutdown => {}
    }
    Ok(())
}

// ── Danger confirmation ───────────────────────────────────────────────────────

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use xaft_config::XaftConfig;

    #[test]
    fn tui_app_constructs() {
        let config = XaftConfig::default();
        let app = TuiApp::new(config);
        let _ = app.theme;
    }
}
