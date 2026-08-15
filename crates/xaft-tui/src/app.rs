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
        let mut prior_lines: Vec<crate::transcript::StyledLine> = Vec::new();
        #[cfg(feature = "session")]
        if let Ok(mgr) = xaft_session::SessionManager::new(&self.config.core.data_dir).await {
            let conv = mgr.conversation_store();

            // On --resume, load prior conversation history for TUI replay.
            if let Some(ref resume_id) = request.resume_session_id {
                use agtrs_runtime::memory::ConversationStore as _;
                let planner_key = format!("{}::workflow::planner", resume_id);
                let planner_msgs = conv.load(&planner_key).await.unwrap_or_default();
                if !planner_msgs.is_empty() {
                    // Also load delegated-agent conversations for full replay.
                    let mut agent_convs: Vec<(String, Vec<agtrs_runtime::transport::Message>)> =
                        Vec::new();
                    for agent in &["coder", "qa", "fixer"] {
                        let key = format!("{}::workflow::{}", resume_id, agent);
                        if let Ok(msgs) = conv.load(&key).await {
                            if !msgs.is_empty() {
                                agent_convs.push(((*agent).to_string(), msgs));
                            }
                        }
                    }
                    prior_lines =
                        replay_history_lines_multi(&planner_msgs, &agent_convs, resume_id);
                } else {
                    // Fallback: try workflow-level or bare session key.
                    for key in &[format!("{}::workflow", resume_id), resume_id.clone()] {
                        if let Ok(msgs) = conv.load(key).await {
                            if !msgs.is_empty() {
                                prior_lines = replay_history_lines(&msgs, resume_id);
                                break;
                            }
                        }
                    }
                }
            }

            runtime = runtime.with_stores(mgr.session_store(), conv);
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
        let (user_msg_tx, mut user_msg_rx) = mpsc::unbounded_channel::<xaft_runtime::UserMessage>();
        let mut state = AppState::new(task.clone());
        state.user_message_tx = Some(user_msg_tx);

        // Replay prior conversation lines (populated above when --resume is set).
        for line in prior_lines {
            state.mutations.push(RenderMutation::CommitLine(line));
        }

        // F3 @-mention: wire the workspace + config + signal bus into
        // AppState. The workspace is an `FsWorkspaceStore` rooted at
        // the working directory so the resolver reads from the same
        // path the runtime will see.
        use agtrs_workspace::WorkspaceStore as _;
        use xaft_tools::FsWorkspaceStore;
        let fws: Arc<FsWorkspaceStore> = Arc::new(FsWorkspaceStore::new(working_dir.clone()));
        let fws_dyn: Arc<dyn agtrs_workspace::WorkspaceStore> = fws.clone();

        // Pre-load the workspace file list for @-mention autocomplete before
        // fws_dyn is moved into init_mention.
        state.mention_file_list = fws_dyn.list().await;

        state.init_mention(
            fws_dyn,
            working_dir.clone(),
            self.config.mention.clone(),
            Some(Arc::clone(&signals)),
        );

        // Load skills for the `$` skill picker (agenthicc parity). This is
        // best-effort: an empty list just leaves the picker with no matches.
        let skills = xaft_skills::SkillLoader::for_working_dir(&working_dir)
            .load_all()
            .await;
        state.init_skills(skills);

        let initial_prompt = build_prompt(&mut state);
        renderer.init_prompt(&initial_prompt, &self.theme)?;

        let mut agent_running = !task.is_empty();
        let mut last_ephemeral = tokio::time::Instant::now();
        const EPHEMERAL_INTERVAL: Duration = Duration::from_millis(100);

        // ── Main loop ─────────────────────────────────────────────────────────
        loop {
            // Accept new task when idle, when the previous task is done, or when
            // the current pipeline has been detached to background (bg_mode=true).
            let can_accept = !agent_running || state.task_done || state.bg_mode;
            if can_accept {
                if let Ok(user_task) = user_msg_rx.try_recv() {
                    // F3 @-mention: the typed input is a `UserMessage`
                    // (Text or MultiPart). For the runtime's `RunRequest.task`
                    // field we use the lossy text view (preserves transcript
                    // display); the structured parts ride along in
                    // `user_message` so the orchestrator can build the
                    // first user turn as `Message::user_with_parts(parts)`.
                    let task_text = user_task.as_text_lossy();
                    let user_message = Some(user_task);
                    state.task = task_text.clone();
                    state.task_done = false;
                    // If sending a new task while bg_mode is active, mark it so
                    // the bg TaskComplete does not prematurely clear agent_running.
                    if state.bg_mode {
                        state.bg_new_task_sent = true;
                    }
                    state.phase = crate::state::WorkflowPhase::Planning;
                    state.reset_for_new_task();

                    // Use the original --resume ID on the first typed task (when the
                    // TUI was opened without an initial task argument). For all
                    // subsequent tasks, use the session that completed most recently.
                    let resume_id = pending_resume_id
                        .take()
                        .or_else(|| state.session.as_ref().map(|s| s.id.to_string()));

                    let mut req = RunRequest {
                        task: task_text,
                        config: self.config.clone(),
                        working_dir: working_dir.clone(),
                        headless: false,
                        dry_run: false,
                        auto_approve: false,
                        dangerously_skip_permissions,
                        resume_session_id: resume_id,
                        workflow: xaft_runtime::WorkflowConfig::default(),
                        prior_messages: vec![],
                        user_message,
                        mode_system_patch: None,
                        mode_tool_filter: None,
                    };
                    state.mode_manager.apply_to_run_request(&mut req);
                    let _ = task_tx.send(req);
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
                let prompt = build_prompt(&mut state);
                renderer.update_prompt(&prompt, &self.theme)?;
            }

            tokio::task::yield_now().await;
        }

        runtime_handle.abort();

        // Clear the bottom block BEFORE printing any post-exit output,
        // so the prompt borders don't appear mixed with the summary.
        renderer.clear_for_exit(&self.theme)?;

        if let Some(hint) = state.exit_resume_hint.take() {
            println!("{hint}");
        }

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

// ── History replay ────────────────────────────────────────────────────────────

/// Convert stored `Message` history into `StyledLine`s for transcript replay.
///
/// Shows User turns as `❯ text` (matches live rendering), Assistant text as
/// indented `AgentText` lines, and tool-use calls as compact `ToolCall` markers.
/// Skips `Role::System` and `Role::Tool` messages (too noisy for display).
fn replay_history_lines(
    messages: &[agtrs_runtime::transport::Message],
    session_id: &str,
) -> Vec<crate::transcript::StyledLine> {
    use crate::transcript::{LineKind, StyledLine};

    let short_id = &session_id[..session_id.len().min(8)];
    let mut out = Vec::new();

    out.push(StyledLine::new(
        format!("  ─── resumed: {short_id} ─────────────────────────────────────────"),
        LineKind::Separator,
    ));

    for msg in messages {
        render_message_into(msg, &mut out, false);
    }

    out
}

/// Find the index just after the planner's final response to a `handoff_to_agent`
/// tool call.  Returns `messages.len()` when no handoff is found (so
/// `&messages[split..]` is empty).
fn find_handoff_split(messages: &[agtrs_runtime::transport::Message]) -> usize {
    use agtrs_runtime::transport::{ContentBlock, MessageContent, Role};

    let mut after_handoff_tool_use = false;
    let mut after_handoff_result = false;

    for (i, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::Assistant => {
                if after_handoff_result {
                    // This is the planner's "I've handed off" reply — split after it.
                    return i + 1;
                }
                // Detect handoff_to_agent tool use in this assistant turn.
                if let MessageContent::MultiPart(blocks) = &msg.content {
                    for block in blocks {
                        if let ContentBlock::ToolUse { name, .. } = block {
                            if name == "handoff_to_agent" {
                                after_handoff_tool_use = true;
                            }
                        }
                    }
                }
            }
            Role::Tool => {
                if after_handoff_tool_use {
                    after_handoff_result = true;
                }
            }
            _ => {}
        }
    }

    messages.len()
}

/// Render a single message into `out`.
///
/// When `skip_user` is true (used for delegated-agent sections), `Role::User`
/// turns are omitted because they contain system context that was piped in by
/// the orchestrator, not the user's own words.
fn render_message_into(
    msg: &agtrs_runtime::transport::Message,
    out: &mut Vec<crate::transcript::StyledLine>,
    skip_user: bool,
) {
    use crate::transcript::{LineKind, StyledLine};
    use agtrs_runtime::transport::{ContentBlock, MessageContent, Role};

    match msg.role {
        Role::User => {
            if skip_user {
                return;
            }
            let text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::MultiPart(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            let file_refs: Vec<&str> = match &msg.content {
                MessageContent::MultiPart(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::FileRef { path, .. } = b {
                            Some(path.as_str())
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => vec![],
            };
            let text = text.trim().to_string();
            if !text.is_empty() {
                out.push(StyledLine::new(format!("❯ {text}"), LineKind::UserMessage));
            }
            for path in file_refs {
                out.push(StyledLine::new(format!("  @{path}"), LineKind::System));
            }
        }

        Role::Assistant => {
            let blocks: Vec<&ContentBlock> = match &msg.content {
                MessageContent::Text(t) => {
                    for line in t.lines() {
                        if !line.trim().is_empty() {
                            out.push(StyledLine::new(format!("  {line}"), LineKind::AgentText));
                        }
                    }
                    return;
                }
                MessageContent::MultiPart(b) => b.iter().collect(),
            };
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        for line in text.lines() {
                            if !line.trim().is_empty() {
                                out.push(StyledLine::new(format!("  {line}"), LineKind::AgentText));
                            }
                        }
                    }
                    ContentBlock::ToolUse { name, input, .. } => {
                        if name != "handoff_to_agent" {
                            let preview =
                                crate::transcript::format_tool_call_inline(name, input, 80);
                            out.push(StyledLine::new(format!("  {preview}"), LineKind::ToolCall));
                        }
                    }
                    _ => {}
                }
            }
        }

        Role::System | Role::Tool => {}
    }
}

/// Full multi-agent conversation replay.
///
/// Splits the planner conversation at the first `handoff_to_agent` call, then
/// inserts each delegated agent's transcript (minus its opening system-context
/// user turn) between the pre- and post-handoff planner sections.
fn replay_history_lines_multi(
    planner_msgs: &[agtrs_runtime::transport::Message],
    agent_convs: &[(String, Vec<agtrs_runtime::transport::Message>)],
    session_id: &str,
) -> Vec<crate::transcript::StyledLine> {
    use crate::transcript::{LineKind, StyledLine};

    let short_id = &session_id[..session_id.len().min(8)];
    let mut out = Vec::new();

    out.push(StyledLine::new(
        format!("  ─── resumed: {short_id} ─────────────────────────────────────────"),
        LineKind::Separator,
    ));

    if agent_convs.is_empty() {
        for msg in planner_msgs {
            render_message_into(msg, &mut out, false);
        }
        return out;
    }

    let split = find_handoff_split(planner_msgs);

    // Pre-handoff planner messages.
    for msg in &planner_msgs[..split] {
        render_message_into(msg, &mut out, false);
    }

    // Delegated-agent sections.
    for (agent_name, msgs) in agent_convs {
        let label = format!(
            "  ──── {} {}──────────────────────────────────",
            agent_name,
            "─".repeat(6usize.saturating_sub(agent_name.len()))
        );
        out.push(StyledLine::new(label, LineKind::Separator));
        // Skip the first user turn — it is the plan/context piped in by the orchestrator.
        let body = if msgs.is_empty() {
            msgs.as_slice()
        } else {
            &msgs[1..]
        };
        for msg in body {
            render_message_into(msg, &mut out, true);
        }
    }

    // Post-handoff planner messages (subsequent user turns, results, etc.).
    if split < planner_msgs.len() {
        out.push(StyledLine::new(
            "  ──── planner (resumed) ────────────────────────────────".to_string(),
            LineKind::Separator,
        ));
        for msg in &planner_msgs[split..] {
            render_message_into(msg, &mut out, false);
        }
    }

    out
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
