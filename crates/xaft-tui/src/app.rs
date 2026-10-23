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
    /// The session's startup-phase timeline (`/startup`, parity row 19). The
    /// CLI hands one in with the `configuration` phase already recorded;
    /// otherwise the TUI starts an empty one.
    startup: xaft_runtime::startup::StartupCoordinator,
}

impl TuiApp {
    /// Create a new `TuiApp` from loaded config.
    pub fn new(config: XaftConfig) -> Self {
        let theme = Theme::from_config(&config.tui.theme);
        Self {
            config,
            theme,
            startup: xaft_runtime::startup::StartupCoordinator::new(),
        }
    }

    /// Continue the startup timeline the caller began (e.g. its timed
    /// `configuration` phase), so `/startup` shows the whole startup rather
    /// than only the part inside the TUI.
    pub fn with_startup(mut self, startup: xaft_runtime::startup::StartupCoordinator) -> Self {
        self.startup = startup;
        self
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
        // An explicit resume is what rehydrates a persisted `/loop`.
        let request_resumed = request.resume_session_id.is_some();
        let cancel = CancellationToken::new();
        let working_dir = request.working_dir.clone();
        let dangerously_skip_permissions = request.dangerously_skip_permissions;

        // ── Event channel ─────────────────────────────────────────────────────
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TuiEvent>();

        // ── Startup phases (`/startup`, parity row 19) ───────────────────────
        //
        // Every phase below wraps real work this function does, so the report
        // is a measurement rather than a list of names. The two discovery
        // phases (workspace index, skills) are deferred until after the first
        // frame, as agenthicc defers extension discovery.
        let startup = self.startup.clone();
        startup.begin("time_to_interactive_input");

        // ── Bootstrap runtime ─────────────────────────────────────────────────
        startup.begin("runtime");
        let mut runtime = match XaftRuntime::bootstrap(self.config.clone()).await {
            Ok(runtime) => {
                startup.finish("runtime");
                runtime
            }
            Err(e) => {
                startup.finish_with(
                    "runtime",
                    xaft_runtime::startup::ReadinessState::Failed,
                    Some(&e.to_string()),
                );
                return Err(e.into());
            }
        };

        // Wire SQLite session + conversation stores if available.
        // This is critical for conversation persistence: without this,
        // the TUI runtime uses FsSessionStore and conversation_store=None,
        // meaning all conversation history is lost between tasks.
        let mut prior_transcript = crate::replay::ResumeTranscript {
            loading: crate::replay::loading_state(),
            chunks: Vec::new(),
        };
        // Stores handed to `AppState` so `/replay` reads the same durable
        // history the runtime writes. `None` when the `session` feature is off
        // or the store could not be opened.
        let mut session_store_for_ui: Option<Arc<dyn xaft_runtime::session_store::SessionStore>> =
            None;
        let mut conversation_store_for_ui: Option<
            Arc<dyn agtrs_runtime::memory::ConversationStore>,
        > = None;
        #[cfg(feature = "session")]
        startup.begin("session_service");
        #[cfg(feature = "session")]
        let session_manager =
            match xaft_session::SessionManager::new(&self.config.core.data_dir).await {
                Ok(mgr) => {
                    startup.finish("session_service");
                    Some(mgr)
                }
                Err(e) => {
                    // The runtime keeps its file-backed session store, so the
                    // session works — with less (no `/replay` history).
                    startup.finish_with(
                        "session_service",
                        xaft_runtime::startup::ReadinessState::Degraded,
                        Some(&format!("SQLite session store unavailable: {e}")),
                    );
                    None
                }
            };
        #[cfg(feature = "session")]
        if let Some(mgr) = session_manager {
            let conv = mgr.conversation_store();

            // On --resume, project the newest N turns of the SESSION LOG.
            //
            // This used to probe `{session}::workflow::planner`, then
            // `{session}::workflow::{agent}` for coder/qa/fixer, then fall back
            // to `{session}::workflow` and finally the bare `{session}` key,
            // rendering whatever it found. That made the resumed transcript
            // depend on which memory keys the orchestrator happened to write
            // rather than on the session log — Gap 4 in
            // `knowledge/tui-parity-gap-analysis.md`.
            //
            // The bare session id IS the conversation key
            // (`xaft_session::conversation_key_for`), so this now reads the
            // same store `/replay` and `xaft session inspect` read. The bound
            // is `tui.resume_transcript_turns` (agenthicc:
            // `behaviour.resume_transcript_turns`, default 20; 0 = all).
            if let Some(ref resume_id) = request.resume_session_id {
                startup.begin("selected_restore");
                let max_turns = self.config.tui.resume_transcript_turns;
                prior_transcript =
                    crate::replay::load_resume_transcript(conv.as_ref(), resume_id, max_turns)
                        .await;
                startup.finish("selected_restore");
            }

            // Hand the TUI's slash commands the SAME two handles the runtime
            // gets below. `/replay` reads real stored history through these;
            // a command context with no store can only report that none is
            // available, which reads identically to a missing feature.
            // ASSIGN, do not shadow: a `let` here would bind a fresh variable the
            // `attach_session_stores` call below cannot see, leaving the TUI
            // with `None` stores and a `/replay` that reports "no store" while
            // the runtime has one.
            session_store_for_ui = Some(mgr.session_store());
            conversation_store_for_ui = Some(Arc::clone(&conv));

            runtime = runtime.with_stores(mgr.session_store(), conv);
            tracing::info!("xaft-tui: SQLite session store active");
        }

        // ── Background terminals: ONE manager for the whole TUI session ──────
        //
        // Built here, the composition root, and handed to BOTH the runtime
        // (`with_terminal_manager`) and `AppState`, so `/ps`, `/stop` and the
        // terminal overlay act on the very terminals the agent's
        // `run_command {"background": true}` started. Two managers must never
        // coexist for one session: `TerminalManager::new` orphans every active
        // record it inherits, so a second one could never stop the first
        // one's terminals.
        //
        // The owner id is the resumed session's id when there is one, so a
        // resumed session sees (as orphaned) the records it left behind. A
        // fresh TUI has no session id until its first run mints one inside the
        // runtime, so it owns its terminals under a TUI-session id minted
        // here — the analogue of agenthicc creating its session id up front.
        let owner_session_id = request
            .resume_session_id
            .clone()
            .unwrap_or_else(|| xaft_runtime::session::SessionId::new().to_string());
        startup.begin("terminal_manager");
        let terminal_manager = Arc::new(xaft_tools::terminals::TerminalManager::new(
            xaft_tools::terminals::TerminalManagerConfig::new(
                owner_session_id.clone(),
                working_dir.clone(),
            ),
        ));
        runtime = runtime.with_terminal_manager(Arc::clone(&terminal_manager));
        startup.finish("terminal_manager");

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
        // The initial run's approval requirement, recorded on `AppState` below
        // so every later run's approvals line up with their own run.
        let initial_approval_filter =
            (!task.is_empty()).then(|| request.mode_approval_filter.clone());
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
        // `/replay` scopes its no-argument form to "sessions for this
        // directory", so it needs the real working directory rather than the
        // `.` placeholder `AppState::new` starts with — otherwise the lookup
        // compares "." against absolute `workspace_root`s and finds nothing.
        state.working_dir = working_dir.clone();
        state.attach_session_stores(session_store_for_ui, conversation_store_for_ui);
        // The SAME manager the runtime was given above.
        state.attach_terminal_manager(Some(Arc::clone(&terminal_manager)));
        if let Some(filter) = initial_approval_filter {
            state.note_run_submitted(filter);
        }

        // ── `/loop` scheduler (parity rows 3/4, `[loops]` rows 143-152) ──────
        //
        // One manager per session, owned under the same id as the terminals.
        // Its store is `core.data_dir/loops`, and a persisted loop is
        // rehydrated only on an explicit resume — agenthicc's
        // `start(rehydrate=bool(ctx.resumed))`. The event loop below ticks the
        // very instance `/loop` and `/loops` act on.
        startup.begin("loop_scheduler");
        let loop_manager = {
            let mut manager = crate::loops::LoopManager::new(
                owner_session_id.clone(),
                self.config.loops.clone(),
                self.config.core.data_dir.join("loops"),
            );
            manager.start(request_resumed);
            Arc::new(std::sync::Mutex::new(manager))
        };
        state.attach_loop_manager(Some(Arc::clone(&loop_manager)));
        startup.finish("loop_scheduler");

        // `/startup` reads the same timeline this function writes, and
        // `/workers` reads the fan-outs this workspace's runs record plus the
        // user-level orchestration store.
        state.attach_startup(Some(startup.clone()));
        state.attach_worker_roots(crate::slash::commands::workers::default_worker_roots(
            &working_dir,
        ));

        // Replay the resumed transcript (populated above when --resume is set).
        //
        // Chunked, behind the `Loading transcript…` status: agenthicc yields
        // between chunks so the loading state is observable and one huge
        // append does not block the first redraw. The status is an EPHEMERAL
        // region, not a committed line, so it disappears once the replay
        // finishes — same lifecycle as agenthicc's `transcript_loading`
        // signal being cleared in its `finally`.
        if !prior_transcript.is_empty() {
            state.transcript_loading = true;
            state
                .mutations
                .push(RenderMutation::SetEphemeral(Some(prior_transcript.loading)));
            for chunk in &prior_transcript.chunks {
                for line in chunk {
                    state
                        .mutations
                        .push(RenderMutation::CommitLine(line.clone()));
                }
                // Yield between chunks so the loading status can be observed,
                // mirroring agenthicc's `await asyncio.sleep(0)`.
                tokio::task::yield_now().await;
            }
            state.transcript_loading = false;
            state.mutations.push(RenderMutation::SetEphemeral(None));
        }

        // F3 @-mention: wire the workspace + config + signal bus into
        // AppState. The workspace is an `FsWorkspaceStore` rooted at
        // the working directory so the resolver reads from the same
        // path the runtime will see.
        use agtrs_workspace::WorkspaceStore as _;
        use xaft_tools::FsWorkspaceStore;
        let fws: Arc<FsWorkspaceStore> = Arc::new(FsWorkspaceStore::new(working_dir.clone()));
        let fws_dyn: Arc<dyn agtrs_workspace::WorkspaceStore> = fws.clone();

        state.init_mention(
            Arc::clone(&fws_dyn),
            working_dir.clone(),
            self.config.mention.clone(),
            Some(Arc::clone(&signals)),
        );

        // First launch installs the curated default skills (agenthicc's
        // `bootstrap_default_skills`, PRD-104): never overwriting a skill and
        // never recreating one the user deleted. Best-effort, like agenthicc.
        let skills_cfg = &self.config.skills;
        let skills_root_setting = skills_cfg.default_skill_directory.clone();
        if let Some(root) = xaft_skills::default_skill_root(&skills_cfg.default_skill_directory) {
            match xaft_skills::bootstrap_default_skills(&root, skills_cfg.install_default_skills) {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!(installed = n, root = %root.display(), "xaft-tui: default skills installed")
                }
                Err(e) => tracing::warn!(error = %e, "xaft-tui: default skill bootstrap failed"),
            }
        }

        // ── Discovery: the workspace file index (`@` autocomplete) and skills
        // (the `$` picker and `/skills`). A fresh session draws its first
        // frame before either runs, as agenthicc defers extension discovery
        // for a fresh interactive session; a resumed session loads them first
        // so the restored conversation has its whole extension surface.
        let (discovery_tx, mut discovery_rx) = mpsc::unbounded_channel::<Discovery>();
        let defer_discovery = !request_resumed;
        if !defer_discovery {
            startup.begin("workspace_index");
            state.mention_file_list = fws_dyn.list().await;
            startup.finish("workspace_index");
            startup.begin("skills");
            let skills = xaft_skills::SkillLoader::for_config(&working_dir, &skills_root_setting)
                .load_all()
                .await;
            state.init_skills(skills);
            startup.finish("skills");
        }

        startup.begin("first_frame");
        let initial_prompt = build_prompt(&mut state);
        if let Err(e) = renderer.init_prompt(&initial_prompt, &self.theme) {
            startup.finish_with(
                "first_frame",
                xaft_runtime::startup::ReadinessState::Failed,
                Some(&e.to_string()),
            );
            return Err(e.into());
        }
        startup.finish("first_frame");
        startup.finish("time_to_interactive_input");

        if defer_discovery {
            startup.begin_deferred("workspace_index");
            {
                let startup = startup.clone();
                let tx = discovery_tx.clone();
                let workspace = Arc::clone(&fws_dyn);
                tokio::spawn(async move {
                    let files = workspace.list().await;
                    startup.finish("workspace_index");
                    let _ = tx.send(Discovery::WorkspaceIndex(files));
                });
            }
            startup.begin_deferred("skills");
            {
                let startup = startup.clone();
                let tx = discovery_tx.clone();
                let dir = working_dir.clone();
                let root = skills_root_setting.clone();
                tokio::spawn(async move {
                    // Best-effort: an empty list just leaves the picker with
                    // no matches.
                    let skills = xaft_skills::SkillLoader::for_config(&dir, &root)
                        .load_all()
                        .await;
                    startup.finish("skills");
                    let _ = tx.send(Discovery::Skills(skills));
                });
            }
        }
        drop(discovery_tx);

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
                    state.note_task_accepted();
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
                        workflow: state.resolved_workflow(),
                        prior_messages: vec![],
                        user_message,
                        mode_system_patch: None,
                        mode_tool_filter: None,
                        mode_approval_filter: None,
                    };
                    state.mode_manager.apply_to_run_request(&mut req);
                    // Safe's approval requirement for this run, so its
                    // approvals are queued for an explicit answer — recorded
                    // per run, because this one may queue behind a run still
                    // going in the background.
                    state.note_run_submitted(req.mode_approval_filter.clone());
                    let _ = task_tx.send(req);
                    agent_running = true;
                    // Tell `/cancel` there is something to cancel.
                    state
                        .intent_running
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    if state.session_start_time.is_none() {
                        state.session_start_time = Some(std::time::Instant::now());
                    }
                }
            }

            // Deferred discovery results land here, on the state's own thread.
            while let Ok(found) = discovery_rx.try_recv() {
                match found {
                    Discovery::WorkspaceIndex(files) => state.mention_file_list = files,
                    Discovery::Skills(skills) => state.init_skills(skills),
                }
            }

            // Drain all pending events.
            while let Ok(event) = event_rx.try_recv() {
                state.handle_event(event);
                for (tool_use_id, approved) in state.pending_gate_decisions.drain(..) {
                    approval_gate.respond(&tool_use_id, approved).await;
                }
            }

            // `/loop`: dispatch a due iteration when the session is idle. It
            // goes out through the same run channel as a typed message, which
            // the accept step at the top of the next pass picks up.
            state.tick_loop_scheduler();

            // Apply render mutations.
            for mutation in state.mutations.drain(..) {
                apply_mutation(renderer, mutation, &self.theme)?;
            }

            // Ephemeral refresh at ~10fps.
            if last_ephemeral.elapsed() >= EPHEMERAL_INTERVAL {
                state.spinner_tick = state.spinner_tick.wrapping_add(1);
                let eph = ephemeral::build_ephemeral(&state);
                renderer.set_ephemeral_opt(eph.as_ref(), &self.theme)?;
                // A live overlay (e.g. `/ps` while a terminal exits) redraws
                // when its projection changes, not only on a keypress.
                if state.refresh_overlay_if_changed() {
                    for mutation in state.mutations.drain(..) {
                        apply_mutation(renderer, mutation, &self.theme)?;
                    }
                }
                last_ephemeral = tokio::time::Instant::now();
            }

            // Ctrl+C timeout check.
            state.check_ctrl_c_timeout();

            // `/cancel` (and its `/interrupt` alias) stops the in-flight turn
            // without quitting. The flag is raised by the slash handler and
            // drained here, so the session stays open and the user can keep
            // working — which is the whole difference from Ctrl+C.
            if state
                .cancel_active_intent
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                cancel.cancel();
                approval_gate.cancel_all().await;
                agent_running = false;
                state
                    .intent_running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                renderer.clear_ephemeral(&self.theme)?;
            }

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
                state
                    .intent_running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                renderer.clear_ephemeral(&self.theme)?;
                let prompt = build_prompt(&mut state);
                renderer.update_prompt(&prompt, &self.theme)?;
            }

            tokio::task::yield_now().await;
        }

        runtime_handle.abort();

        // Session teardown stops every live terminal this session owns
        // (agenthicc's `TerminalManager.close()`), so a background server the
        // agent started does not outlive the TUI as an unowned process group.
        // Bounded: `close` waits out each terminal's grace period, and exit
        // must not hang on a process that ignores its signals.
        let _ = tokio::time::timeout(Duration::from_secs(10), terminal_manager.close()).await;
        // Anything still loading is cancelled with the session.
        startup.close();
        // Stop scheduling; the persisted loop stays for an explicit resume.
        loop_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown();

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

        // `tui.verbose` (parity row 140). Independent of `show_exit_summary`:
        // the summary is the human-facing recap, this is the diagnostic line.
        // Emitted whether or not the summary was, so turning verbose on is
        // observable on its own rather than only as a side effect of the
        // summary flag.
        if let Some(start) = state.session_start_time {
            if let Some(line) = exit_diagnostics_line(
                self.config.tui.verbose,
                start.elapsed(),
                state.total_input_tokens,
                state.total_output_tokens,
                state.total_cost_usd,
                state.session.as_ref().map(|s| s.id.to_string()).as_deref(),
            ) {
                println!("{line}");
            }
        }

        Ok(())
    }
}

/// A deferred discovery result, delivered to the event loop.
enum Discovery {
    /// The workspace file list for `@` autocomplete.
    WorkspaceIndex(Vec<String>),
    /// The loaded skills, for the `$` picker and `/skills`.
    Skills(Vec<xaft_skills::Skill>),
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

/// The post-exit diagnostics line printed when `tui.verbose` is on.
///
/// Parity row 140 (`BehaviourSettings.verbose`). This exists so the key has a
/// reader OUTSIDE `xaft-config` and changes observable behaviour: a config
/// field nothing reads is dead config, which is worse than a missing key
/// because it looks implemented. Commit e9939e3 declared the field with no
/// consumer, and `scripts/config-key-audit.py` correctly scored it `❌` as
/// a dead field.
///
/// It is a PURE function rather than a `println!` inside the event loop so the
/// behaviour it gates is directly assertable, not something a test has to
/// scrape out of stdout. The caller prints whatever this returns; it returns
/// `None` when the key is off.
pub fn exit_diagnostics_line(
    verbose: bool,
    elapsed: std::time::Duration,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    session_id: Option<&str>,
) -> Option<String> {
    if !verbose {
        return None;
    }
    Some(format!(
        "[verbose] session={} elapsed={}s tokens={} in/{} out cost=${:.4}",
        session_id.unwrap_or("<none>"),
        elapsed.as_secs(),
        input_tokens,
        output_tokens,
        cost_usd,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{LineKind, LineStyle, StyledLine};
    use std::time::Duration;
    use xaft_config::XaftConfig;

    #[test]
    fn tui_app_constructs() {
        let config = XaftConfig::default();
        let app = TuiApp::new(config);
        let _ = app.theme;
    }

    /// The regression test for the dead-config defect: turning the key OFF
    /// must produce nothing at all, and turning it ON must produce a line
    /// carrying the session's real numbers. Delete the wiring and the second
    /// assertion fails.
    #[test]
    fn the_verbose_key_gates_the_exit_diagnostics_line() {
        let off = exit_diagnostics_line(false, Duration::from_secs(12), 10, 20, 0.5, Some("s1"));
        assert!(
            off.is_none(),
            "verbose=false must print nothing, got {off:?}"
        );

        let on = exit_diagnostics_line(true, Duration::from_secs(12), 10, 20, 0.5, Some("s1"))
            .expect("verbose=true must print a line");
        assert!(on.contains("session=s1"), "{on}");
        assert!(on.contains("elapsed=12s"), "{on}");
        assert!(on.contains("10 in/20 out"), "{on}");
        assert!(on.contains("0.5000"), "{on}");
    }

    /// A session that never got an id must still print a well-formed line
    /// rather than `session=None` or a bare `session=`.
    #[test]
    fn a_missing_session_id_renders_as_a_none_marker() {
        let line = exit_diagnostics_line(true, Duration::from_secs(0), 0, 0, 0.0, None)
            .expect("verbose=true prints regardless of the id");
        assert!(line.contains("session=<none>"), "{line}");
        assert!(line.contains("elapsed=0s"), "{line}");
        assert!(line.contains("0 in/0 out"), "{line}");
    }

    /// The value must be readable from the config the app is actually given.
    /// That is the whole difference between a wired field and a declared one.
    #[test]
    fn the_verbose_key_is_read_from_the_apps_own_config() {
        let mut on = XaftConfig::default();
        on.tui.verbose = true;
        let app = TuiApp::new(on);
        assert!(
            app.config.tui.verbose,
            "TuiApp must carry the configured value"
        );
        assert!(
            exit_diagnostics_line(
                app.config.tui.verbose,
                Duration::from_secs(1),
                0,
                0,
                0.0,
                None
            )
            .is_some()
        );

        let mut off = XaftConfig::default();
        off.tui.verbose = false;
        let app = TuiApp::new(off);
        assert!(
            exit_diagnostics_line(
                app.config.tui.verbose,
                Duration::from_secs(1),
                0,
                0,
                0.0,
                None
            )
            .is_none()
        );
    }

    // ── Event-loop driver ────────────────────────────────────────────────
    //
    // `run_inner` is private and concretely typed as `&mut IncrementalRenderer`
    // (= `IncrementalRenderer<BufWriter<Stdout>>`), so the only renderer a unit
    // test can hand it is `IncrementalRenderer::new()`. That is safe with no
    // tty: renderer.rs never calls `enable_raw_mode` (the call that needs a
    // pty) and its only crossterm queries both fall back --
    // `terminal::size().unwrap_or((120, 40))` and a `disable_raw_mode()` whose
    // result is discarded. The escapes it writes go to stdout, which libtest
    // captures per test.

    /// Drive the real event loop end to end.
    ///
    /// Exit is deterministic through the loop's OWN quit seam -- the
    /// `Arc<AtomicBool>` the caller threads in -- rather than by aborting the
    /// task, so the post-loop teardown also runs: the runtime handle abort,
    /// `clear_for_exit`, the exit hint, the exit summary and the `tui.verbose`
    /// diagnostic line. The 300 ms delay is longer than `EPHEMERAL_INTERVAL`
    /// (100 ms) so the ~10 fps ephemeral refresh branch executes first.
    ///
    /// The `timeout` is the regression guard: if the quit seam ever stops
    /// working, this test FAILS instead of hanging the suite.
    #[tokio::test]
    async fn run_inner_reaches_the_event_loop_and_exits_through_the_sigint_seam() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = XaftConfig::default();
        // Keep the runtime's FsSessionStore and the SQLite SessionManager out
        // of the real home directory.
        config.core.data_dir = dir.path().to_path_buf();
        // Both post-exit reporters on, so the teardown assertions below have
        // something to attribute to the config rather than to a default.
        config.tui.show_exit_summary = true;
        config.tui.verbose = true;

        let app = TuiApp::new(config.clone());
        let mut renderer = IncrementalRenderer::new().expect("stdout renderer");

        let request = xaft_runtime::RunRequest {
            // Empty task: the loop must still come up, and no provider call is
            // made (`agent_running` stays false and nothing is pushed onto the
            // task channel).
            task: String::new(),
            config,
            working_dir: dir.path().to_path_buf(),
            headless: false,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
            resume_session_id: None,
            workflow: xaft_runtime::WorkflowConfig::default(),
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
            mode_approval_filter: None,
        };

        let sigint_received = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&sigint_received);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            flag.store(true, Ordering::SeqCst);
        });

        let outcome = tokio::time::timeout(
            Duration::from_secs(60),
            app.run_inner(request, sigint_received, &mut renderer, true),
        )
        .await
        .expect("run_inner must reach its quit check instead of hanging");

        assert!(outcome.is_ok(), "run_inner failed: {outcome:?}");
    }

    /// The loop's mutation dispatcher.
    ///
    /// `apply_mutation` is private to this module and concretely typed, so it
    /// is only reachable from here. The three stream arms are asserted through
    /// their OBSERVABLE effect (`stream_line_open`) rather than by "it returned
    /// Ok" -- a dispatcher that routed a stream token to the wrong renderer
    /// method, or dropped it, would still return Ok.
    #[test]
    fn apply_mutation_routes_every_variant_to_the_renderer() {
        use crate::ephemeral::EphemeralState;
        use crate::prompt::PromptState;

        let theme = Theme::dark();
        let mut renderer = IncrementalRenderer::new().expect("stdout renderer");

        apply_mutation(
            &mut renderer,
            RenderMutation::CommitLine(StyledLine::new("committed", LineKind::AgentText)),
            &theme,
        )
        .expect("CommitLine must dispatch");
        assert!(!renderer.stream_line_open);

        apply_mutation(
            &mut renderer,
            RenderMutation::StreamToken {
                fragment: "streamed".to_string(),
                style: LineStyle::Normal,
            },
            &theme,
        )
        .expect("StreamToken must dispatch");
        assert!(
            renderer.stream_line_open,
            "StreamToken must reach update_stream"
        );

        // A commit while a stream is open must close the stream line -- that is
        // the routing proof for the CommitLine arm.
        apply_mutation(
            &mut renderer,
            RenderMutation::CommitLine(StyledLine::new("after", LineKind::AgentText)),
            &theme,
        )
        .expect("CommitLine must dispatch");
        assert!(
            !renderer.stream_line_open,
            "a commit must close the open stream line"
        );

        apply_mutation(&mut renderer, RenderMutation::FlushStream, &theme)
            .expect("FlushStream must dispatch");

        let eph = EphemeralState {
            spinner_line: "✦ Planning…".to_string(),
            status_line: Some("Tokens: 1 in / 2 out".to_string()),
        };
        apply_mutation(
            &mut renderer,
            RenderMutation::SetEphemeral(Some(eph)),
            &theme,
        )
        .expect("SetEphemeral(Some) must dispatch");
        apply_mutation(&mut renderer, RenderMutation::SetEphemeral(None), &theme)
            .expect("SetEphemeral(None) must dispatch");

        apply_mutation(
            &mut renderer,
            RenderMutation::UpdatePrompt(PromptState::default()),
            &theme,
        )
        .expect("UpdatePrompt must dispatch");

        apply_mutation(
            &mut renderer,
            RenderMutation::Resize {
                cols: 100,
                rows: 30,
            },
            &theme,
        )
        .expect("Resize must dispatch");

        // Shutdown is the documented no-op arm; the renderer must survive the
        // loop reaching it and be usable afterwards.
        apply_mutation(&mut renderer, RenderMutation::Shutdown, &theme)
            .expect("Shutdown must dispatch");
        assert!(
            !renderer.stream_line_open,
            "Shutdown must leave the renderer usable"
        );
    }
}
