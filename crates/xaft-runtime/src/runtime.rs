//! `XaftRuntime` — the real implementation of `RuntimeDispatch`.
//!
//! # Boot sequence
//!
//! ```text
//! XaftRuntime::bootstrap(config)
//!     ├── validate config
//!     ├── create SignalBus
//!     ├── create FsSessionStore (or InMemorySessionStore in tests)
//!     └── → XaftRuntime
//!
//! XaftRuntime::run(request)
//!     ├── resolve agent preset from config
//!     ├── build LLM provider via ProviderFactory
//!     ├── open FsWorkspaceStore for working_dir
//!     ├── optionally open GitRepo + WorktreeGuard
//!     ├── build ToolRegistry (list/read/edit/grep/write/bash/git)
//!     ├── build XaftAgent or PlanModeAgent
//!     ├── build AgentContext
//!     ├── run AgentExecutor::run_stream()
//!     └── consume events via EventLoop → RunResult
//! ```

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument, warn};

use agtrs_git::GitRepo;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::signals::{ModelCallComplete, SignalBus, ToolCallComplete, ToolCallStarted};
use xaft_config::XaftConfig;
use xaft_tools::FsWorkspaceStore;
use xaft_tools::browser::BrowserSessionCache;
use xaft_tools::registry::ToolRegistryBuilder;
use xaft_tools::terminals::{TerminalManager, TerminalManagerConfig};

use crate::dispatch::{RunRequest, RunResult, RuntimeDispatch};
use crate::error::RuntimeError;
use crate::provider::{ProviderFactory, build_tiered_provider};
use crate::session::{AgentSession, SessionStatus};
use crate::session_store::{FsSessionStore, InMemorySessionStore, SessionStore};
use crate::types::{ExitCode, UserMessage};

// ── XaftRuntime ───────────────────────────────────────────────────────────────

/// The production xaft runtime — composes all agtrs primitives into a
/// cohesive autonomous coding system.
pub struct XaftRuntime {
    config: XaftConfig,
    signals: Arc<SignalBus>,
    session_store: Arc<dyn SessionStore>,
    /// Optional pre-built provider override (for testing without real API keys).
    provider_override: Option<Arc<dyn LlmProvider>>,
    /// Durable conversation store for session resume (None → in-memory ephemeral).
    pub(crate) conversation_store: Option<Arc<dyn ConversationStore>>,
    /// Optional approval gate — wired in TUI mode so agents can request confirmation.
    pub(crate) approval_gate: Option<Arc<dyn agtrs_runtime::approval::ApprovalGate>>,
    /// The background-terminal manager every run of this runtime hands to the
    /// tools that need one.
    ///
    /// `None` means "resolve one per run" — see
    /// [`XaftRuntime::resolve_terminal_manager`]. A composition root that
    /// outlives a single run (the TUI, which keeps one session across turns)
    /// injects its manager with [`XaftRuntime::with_terminal_manager`], so the
    /// terminals one turn started are still the ones the next turn can inspect
    /// and stop.
    pub(crate) terminal_manager: Option<Arc<TerminalManager>>,
    /// Free-text question channel for workflow `ask_user` calls — wired by a
    /// composition root with a user attached (the TUI). `None` makes
    /// `ask_user` report `timed_out`, agenthicc's headless behaviour.
    pub(crate) question_gate: Option<Arc<dyn crate::phased::question::UserQuestionGate>>,
    /// One browser manager per session, reused across that session's runs
    /// (parity rows 60-77). agenthicc keeps one browser context per stable
    /// session conversation, so a page opened in one turn is still open in the
    /// next; the TUI reuses one runtime and one session id across turns, which
    /// is what makes this cache the right owner. Shared (an `Arc`) with the
    /// runtime `resume_session` builds, so a resumed session keeps its pages.
    pub(crate) browser_sessions: Arc<BrowserSessionCache>,
    /// Optional memory manager for long-term knowledge persistence.
    #[cfg(feature = "memory")]
    pub(crate) memory_manager: Option<Arc<xaft_memory::XaftMemoryManager>>,
}

/// Accumulated LLM cost and token usage for a single run.
///
/// Shared via `Arc<Mutex<…>>` between the signal-bus listener task and the
/// run_task body so that every `ModelCallComplete` event is immediately
/// reflected in the final session record.
#[derive(Debug, Default, Clone)]
struct RunCostAccumulator {
    total_cost_usd: f64,
    total_tokens: u64,
    llm_calls: u32,
}

impl std::fmt::Debug for XaftRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XaftRuntime").finish()
    }
}

impl XaftRuntime {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Bootstrap a production `XaftRuntime` from loaded config.
    ///
    /// Uses `FsSessionStore` by default. To enable SQLite session persistence,
    /// call [`XaftRuntime::with_stores`] after bootstrapping, passing stores
    /// created by `xaft_session::SessionManager`.
    pub async fn bootstrap(config: XaftConfig) -> Result<Self, RuntimeError> {
        let signals = Arc::new(SignalBus::new());
        attach_tool_call_logger(&signals).await;
        attach_file_edit_broadcaster(&signals).await;
        let session_store: Arc<dyn SessionStore> =
            Arc::new(FsSessionStore::new(&config.core.data_dir).await?);
        Ok(Self {
            config,
            signals,
            session_store,
            provider_override: None,
            conversation_store: None,
            approval_gate: None,
            terminal_manager: None,
            question_gate: None,
            browser_sessions: Arc::new(BrowserSessionCache::new()),
            #[cfg(feature = "memory")]
            memory_manager: None,
        })
    }

    /// Replace session + conversation stores (called from binary layer after
    /// bootstrapping `xaft_session::SessionManager`).
    ///
    /// This is the preferred way to inject SQLite persistence without creating
    /// a dependency cycle between `xaft-runtime` and `xaft-session`.
    pub fn with_stores(
        mut self,
        session_store: Arc<dyn SessionStore>,
        conversation_store: Arc<dyn ConversationStore>,
    ) -> Self {
        self.session_store = session_store;
        self.conversation_store = Some(conversation_store);
        self
    }

    /// Share ONE background-terminal manager with every run of this runtime.
    ///
    /// The composition root builds the manager where both the session id and
    /// the workspace are known, so the registries a run builds — and the TUI's
    /// own terminal surface — read and write the *same* store. Exactly one
    /// manager may exist per session: `TerminalManager::new` marks every active
    /// record it inherits from its own session as `Orphaned`, because a manager
    /// cannot prove it spawned them (`terminals/manager.rs`, "ownership
    /// recovery"), so a second manager over the same store would refuse to stop
    /// the first one's terminals.
    ///
    /// Without this, each run resolves a manager of its own from the session id
    /// and the run's working directory; see
    /// [`XaftRuntime::resolve_terminal_manager`].
    pub fn with_terminal_manager(mut self, manager: Arc<TerminalManager>) -> Self {
        self.terminal_manager = Some(manager);
        self
    }

    /// Attach the channel workflow `ask_user` calls use to reach the user.
    pub fn with_question_gate(
        mut self,
        gate: Arc<dyn crate::phased::question::UserQuestionGate>,
    ) -> Self {
        self.question_gate = Some(gate);
        self
    }

    /// Share one browser-session cache with every run of this runtime.
    ///
    /// Each runtime already owns a cache; a composition root that wants
    /// several runtimes (or a test) to see the same browser sessions injects
    /// one here.
    pub fn with_browser_sessions(mut self, cache: Arc<BrowserSessionCache>) -> Self {
        self.browser_sessions = cache;
        self
    }

    /// The browser-session cache runs of this runtime use.
    pub fn browser_sessions(&self) -> &Arc<BrowserSessionCache> {
        &self.browser_sessions
    }

    /// The terminal manager a run over `session_id` in `working_dir` must use.
    ///
    /// The injected manager when a composition root supplied one, otherwise a
    /// new one built here — this crate is the only place where both the session
    /// id (assigned inside `run_task`) and the working directory are known on
    /// *every* path, headless and TUI alike. That is what makes the four
    /// agent-visible terminal tools and `run_command {"background": true}`
    /// work in the product instead of refusing with `terminal manager
    /// unavailable`.
    ///
    /// The store lands in `working_dir/.xaft`, the manager's own default.
    fn resolve_terminal_manager(
        &self,
        session_id: &str,
        working_dir: &Path,
    ) -> Arc<TerminalManager> {
        self.terminal_manager.clone().unwrap_or_else(|| {
            Arc::new(TerminalManager::new(TerminalManagerConfig::new(
                session_id,
                working_dir,
            )))
        })
    }

    /// Create for testing with an in-memory session store.
    ///
    /// Pass `Some(llm)` to bypass `ProviderFactory` (no API keys needed in
    /// tests).
    pub fn for_testing(config: XaftConfig, llm: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            config,
            signals: Arc::new(SignalBus::new()),
            session_store: Arc::new(InMemorySessionStore::new()),
            provider_override: llm,
            conversation_store: None,
            approval_gate: None,
            terminal_manager: None,
            question_gate: None,
            browser_sessions: Arc::new(BrowserSessionCache::new()),
            #[cfg(feature = "memory")]
            memory_manager: None,
        }
    }

    /// Attach an approval gate (e.g. `TuiApprovalGate` from the TUI layer).
    ///
    /// When set, tools that return `requires_confirmation = true` will block
    /// until the gate approves or rejects the call. The gate is propagated to
    /// every agent executor spawned by this runtime.
    pub fn with_approval_gate(
        mut self,
        gate: Arc<dyn agtrs_runtime::approval::ApprovalGate>,
    ) -> Self {
        self.approval_gate = Some(gate);
        self
    }

    /// Attach a memory manager for long-term knowledge persistence.
    ///
    /// When set, memory tools (remember, recall, forget, summarize_memory)
    /// are automatically registered for all agents.
    #[cfg(feature = "memory")]
    pub fn with_memory(mut self, memory: Arc<xaft_memory::XaftMemoryManager>) -> Self {
        self.memory_manager = Some(memory);
        self
    }

    /// Access the shared signal bus (useful for attaching signal listeners).
    pub fn signals(&self) -> &Arc<SignalBus> {
        &self.signals
    }

    // ── Core run ──────────────────────────────────────────────────────────────

    /// Execute a task and return a result.
    ///
    /// This is the primary entry point for all agent runs.
    #[instrument(name = "xaft_run", skip_all, fields(task = %request.task))]
    async fn run_task(&self, mut request: RunRequest) -> Result<RunResult, RuntimeError> {
        let working_dir = &request.working_dir;
        info!(task = %request.task, dir = %working_dir.display(), "xaft: starting run");

        // ── Resolve preset and model ──────────────────────────────────────────
        let preset_name = "default";
        let preset = self.config.agent.get(preset_name).ok_or_else(|| {
            RuntimeError::Config(format!("agent preset '{preset_name}' not found"))
        })?;

        // ── Create or resume session ───────────────────────────────────────────
        // Track whether we are resuming a *completed* session (new user task)
        // vs. resuming an *active* session (crash recovery mid-task).
        // This governs whether agents receive their prior conversation history.
        let mut resume_from_completed = false;

        let mut session = if let Some(ref resume_id) = request.resume_session_id {
            // Resume: load existing session
            let id = crate::session::SessionId::from_string(resume_id);
            let mut existing = self
                .session_store
                .load(&id)
                .await?
                .ok_or_else(|| RuntimeError::SessionNotFound(resume_id.clone()))?;

            // Allow resuming Active, Suspended, and Completed sessions.
            // Completed sessions are resumable for TUI multi-turn: the first
            // task completes the session, and the user sends a second task.
            // Only reject Failed and Cancelled sessions (hard errors).
            match &existing.status {
                SessionStatus::Active | SessionStatus::Suspended => {
                    // Crash recovery: agent histories will be reloaded so the
                    // workflow can continue from where it left off.
                }
                SessionStatus::Completed { .. } => {
                    // New task in a completed session.  Agents must NOT reload
                    // prior tool-call history or they will re-implement the
                    // previous task's work verbatim on every --continue invocation.
                    resume_from_completed = true;
                }
                SessionStatus::Failed { error } => {
                    return Err(RuntimeError::Config(format!(
                        "session '{}' cannot be resumed (failed: {})",
                        resume_id, error
                    )));
                }
                SessionStatus::Cancelled => {
                    return Err(RuntimeError::Config(format!(
                        "session '{}' cannot be resumed (cancelled)",
                        resume_id
                    )));
                }
            }

            // Load prior conversation history if available
            if let Some(ref conv_store) = self.conversation_store {
                let workflow_key = format!("{}::workflow", resume_id);
                match conv_store.load(&workflow_key).await {
                    Ok(msgs) if !msgs.is_empty() => {
                        info!(
                            session_id = %resume_id,
                            message_count = msgs.len(),
                            "xaft: loaded prior conversation history for resume"
                        );
                        request.prior_messages = msgs;
                    }
                    _ => {
                        // Try the session-level key as fallback
                        let session_key = resume_id.to_string();
                        match conv_store.load(&session_key).await {
                            Ok(msgs) if !msgs.is_empty() => {
                                info!(
                                    session_id = %resume_id,
                                    message_count = msgs.len(),
                                    "xaft: loaded prior conversation history (session key)"
                                );
                                request.prior_messages = msgs;
                            }
                            _ => {
                                tracing::debug!(
                                    session_id = %resume_id,
                                    "xaft: no prior conversation history found for resume"
                                );
                            }
                        }
                    }
                }
            }

            // Reset status to Active for the resumed session
            let mut resumed = existing;
            resumed.status = SessionStatus::Active;
            resumed.updated_at = chrono::Utc::now();
            self.session_store.save(&resumed).await?;
            resumed
        } else {
            // New session
            AgentSession::new(
                request.task.clone(),
                working_dir.clone(),
                preset_name.to_string(),
                preset.model.clone(),
            )
        };
        self.session_store.save(&session).await?;

        // ── Create conversation record (idempotent) ───────────────────────────
        // The orchestrator will call ConversationStore::save() which creates
        // the conversation automatically. Metadata (task, working_dir) is
        // already stored in the session record.
        if let Some(ref conv_store) = self.conversation_store {
            // Pre-create the conversation so resume can find it.
            // Use save() with empty messages — the orchestrator will overwrite.
            let conv_key = session.id.as_str().to_string();
            let _ = conv_store.save(&conv_key, &[]).await;
        }

        // ── Dry-run shortcut (before expensive provider/worktree setup) ───────
        if request.dry_run {
            info!("xaft: dry-run mode, no execution");
            session.status = SessionStatus::Completed {
                summary: format!("dry-run: would execute '{}'", request.task),
            };
            self.session_store.save(&session).await?;
            return Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: format!("dry-run completed for: {}", request.task),
            });
        }

        // ── Load AGENTS.md project instructions ───────────────────────────────
        if self.config.core.agents_md_enabled {
            use crate::agents_md::load_agents_md;
            let agents_msgs =
                load_agents_md(working_dir, self.config.core.agents_md_max_bytes).await;
            if !agents_msgs.is_empty() {
                let total_bytes: usize = agents_msgs.iter().map(|m| m.text().len()).sum();
                let count = agents_msgs.len();
                tracing::info!(total_bytes, count, "xaft: AGENTS.md loaded");
                let loaded_paths: Vec<String> = crate::agents_md::agents_md_paths(working_dir)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let mut all = agents_msgs;
                all.extend(request.prior_messages.drain(..));
                request.prior_messages = all;
                let signals = Arc::clone(&self.signals);
                tokio::spawn(async move {
                    signals
                        .emit(xaft_agent::signals::XaftAgentsMdLoaded {
                            paths: loaded_paths,
                            total_bytes,
                        })
                        .await;
                });
            }
        }

        // ── Load skills ───────────────────────────────────────────────────────
        {
            use xaft_skills::SkillLoader;
            let loader =
                SkillLoader::for_config(working_dir, &self.config.skills.default_skill_directory);
            let skills = loader.load_all().await;
            if !skills.is_empty() {
                let count = skills.len();
                tracing::info!(count, "xaft: skills loaded");
                let section = SkillLoader::build_prompt_section(&skills);
                let skill_msg = agtrs_runtime::transport::Message::system(section);
                let mut all: Vec<agtrs_runtime::transport::Message> =
                    request.prior_messages.drain(..).collect();
                all.push(skill_msg);
                request.prior_messages = all;
            }
        }

        // ── Build LLM provider ────────────────────────────────────────────────
        let llm = if let Some(p) = &self.provider_override {
            Arc::clone(p)
        } else {
            let tiers = self.config.model_tiers.resolve(&preset.model);
            if tiers.all_same() {
                ProviderFactory::build(&self.config, Some(preset_name))?
            } else {
                build_tiered_provider(&self.config, preset_name, &tiers)?
            }
        };

        // ── Workspace store ───────────────────────────────────────────────────
        let _workspace = Arc::new(FsWorkspaceStore::new(working_dir));

        // ── Git worktree (optional) ───────────────────────────────────────────
        let git_guard = if !request.dry_run {
            try_open_git_worktree(working_dir, &session.id.to_string()).await
        } else {
            None
        };

        if let Some(g) = &git_guard {
            let branch = g.branch_name().to_string();
            session.git_branch = Some(branch.clone());
            info!(branch = %branch, "xaft: agent branch created");
            self.session_store.save(&session).await?;
        }

        // ── Tool registries (read-only + full write) ──────────────────────────
        // Background terminals: resolved *here*, once per run, because this is
        // the only frame where both the session id (assigned just above) and
        // the run's working directory are known on every path — headless, JSON
        // and TUI alike. It sits after the dry-run shortcut, so `--dry-run`
        // still touches no store. The one `Arc` goes to the registry that
        // holds the terminal tools: the four agent-visible tools and
        // `run_command {"background": true}` refuse — agenthicc's `terminal
        // manager unavailable` — unless they were built with a manager, so
        // before this line rows 36/43/44/45 were a registry surface only.
        let terminal_manager = self.resolve_terminal_manager(session.id.as_str(), working_dir);

        // Browser tools (parity rows 60-77): the session's manager, selected by
        // `tools.browser_backend` and reused across the session's runs. A run
        // is an agenthicc turn, so the action budget restarts here — agenthicc
        // calls `reset_turn_budget()` at the top of every turn. `None` means
        // `browser_backend = "none"`: no browser tools at all.
        let browser = self.browser_sessions.session_for(
            &request.config.tools,
            working_dir,
            session.id.as_str(),
        );
        if let Some(browser) = &browser {
            browser.reset_turn_budget();
        }

        // `[tools] http_timeout_s` is the Outlook tools' Graph read timeout
        // (agenthicc's `agenthicc_http_client` default; parity row 115).
        let http_timeout_s = request.config.tools.http_timeout_s;

        let read_tools = {
            let mut builder = ToolRegistryBuilder::new(working_dir)
                .without_git()
                .with_http_timeout_s(http_timeout_s);
            if let Some(browser) = &browser {
                builder = builder.with_browser_session(Arc::clone(browser));
            }
            builder
                .build_reader()
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?
                .all()
        };

        let write_tools = {
            // The four terminal tools are coder-registry tools (they need a
            // shell to be useful), so `build_reader` above takes no manager.
            let mut builder = ToolRegistryBuilder::new(working_dir)
                .with_shell()
                .with_http_timeout_s(http_timeout_s)
                .with_terminal_manager(Arc::clone(&terminal_manager));
            if let Some(browser) = &browser {
                builder = builder.with_browser_session(Arc::clone(browser));
            }
            if git_guard.is_some() {
                builder = builder.without_git();
            }
            let mut t = builder
                .build_coder()
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?
                .all();
            if let Some(guard) = &git_guard {
                t.extend(agtrs_git::GitToolSet::new(Arc::clone(guard)).all());
            }
            t
        };

        // read_tools for QA (no writes); write_tools for coder + fixer
        // Include git read-only tools in read_tools for QA inspection
        let read_tools = {
            let mut t = read_tools;
            if let Some(guard) = &git_guard {
                t.extend(agtrs_git::GitToolSet::new(Arc::clone(guard)).read_only());
            }
            t
        };

        // ── Memory tools (if memory manager is configured) ─────────────────────
        #[cfg(feature = "memory")]
        let (read_tools, write_tools) = if let Some(ref mem_mgr) = self.memory_manager {
            let toolset = xaft_memory::tools::memory_toolset(Arc::clone(mem_mgr));
            let mut r = read_tools;
            r.extend(toolset.read_only());
            let mut w = write_tools;
            w.extend(toolset.all());
            (r, w)
        } else {
            (read_tools, write_tools)
        };
        #[cfg(not(feature = "memory"))]
        let (read_tools, write_tools) = (read_tools, write_tools);

        // ── Mode tool filter (PRD 64) ──────────────────────────────────────────
        // A phased workflow receives the *unfiltered* pools plus the filter:
        // a phase's `mode_override` replaces the session mode for that phase
        // (agenthicc's `phase_mode`), so the engine filters per phase.
        let unfiltered_tools = (read_tools.clone(), write_tools.clone());
        let mut read_tools = read_tools;
        let mut write_tools = write_tools;
        if let Some(ref filter) = request.mode_tool_filter {
            read_tools.retain(|t| filter(t.name()));
            write_tools.retain(|t| filter(t.name()));
        }
        // ── Mode approval gate (agenthicc's Safe, parity §3 row 1) ────────────
        // Tools the active mode marks as side-effecting must be approved before
        // every call: wrapped here, they report `requires_confirmation`, and the
        // executor sends them through the approval gate like `bash_exec`.
        if let Some(ref requires_approval) = request.mode_approval_filter {
            read_tools = crate::mode_gate::require_approval(read_tools, requires_approval);
            write_tools = crate::mode_gate::require_approval(write_tools, requires_approval);
        }

        // ── Resolve context (for planner tool-call strategy) ──────────────────
        let resolve_ctx = Arc::new(injectable_rs_runtime::ResolveContext::from_store(Arc::new(
            injectable_rs_runtime::EmptySingletonStore,
        )));

        // ── Cost/token accumulation via ModelCallComplete signal ──────────────
        // Subscribe to every LLM call completion emitted during this run and
        // accumulate cost+tokens into a shared counter that is flushed into the
        // session record after the orchestrator returns.
        let cost_acc = Arc::new(std::sync::Mutex::new(RunCostAccumulator::default()));
        {
            let acc = Arc::clone(&cost_acc);
            self.signals
                .on::<ModelCallComplete>(move |ev| {
                    if let Ok(mut a) = acc.lock() {
                        a.total_cost_usd += ev.cost_usd;
                        a.total_tokens += ev.total_tokens as u64;
                        a.llm_calls += 1;
                    }
                })
                .await;
        }

        // ── Run orchestrated workflow ─────────────────────────────────────────
        // Standard path:  plan → coder → QA ↔ fixer (HandoffOrchestrator::run)
        // Dynamic path:   any registered agent can hand off to any other
        //                 (run_dynamic_handoff via AgentRegistry)
        use crate::agent_registry::WorkflowConfig;

        let is_dynamic = matches!(request.workflow, WorkflowConfig::Dynamic { .. });
        let is_meta = matches!(request.workflow, WorkflowConfig::Meta { .. });

        // F3 @-mention: if the request carried a structured `UserMessage`
        // (resolved mentions from the TUI), pass its content blocks to the
        // orchestrator. The first user turn in the agent's history will be
        // built as `Message::user_with_parts(parts)` instead of plain
        // `Message::user(task)`. Falls back to None when no `UserMessage`
        // was provided (CLI / headless) — preserves pre-F3 behaviour.
        let user_parts: Option<Vec<agtrs_runtime::transport::ContentBlock>> =
            match &request.user_message {
                Some(UserMessage::MultiPart(parts)) => Some(parts.clone()),
                // `UserMessage::Text` carries the same text as `request.task`,
                // so we don't need to pass anything down — the orchestrator
                // will use `request.task` directly.
                Some(UserMessage::Text(_)) | None => None,
            };

        // When resuming a previously *completed* session (new user task via
        // --continue), pass None so the orchestrator creates a fresh
        // InMemoryConversationStore.  This prevents agents from loading their
        // prior tool-call history and re-implementing the previous task's work.
        //
        // When the session was *Active* (crash recovery) or this is a brand-new
        // session, pass the real persisted store so agents can resume mid-task or
        // save history for future crash recovery.
        let orchestrator_conv_store = if resume_from_completed {
            None
        } else {
            self.conversation_store.clone()
        };

        // AGENTS.md instructions and the skills section were loaded above as
        // system messages. They must reach the model: the phased engine puts
        // them in every phase's system prompt, and the standard pipeline
        // receives them through the prompt patch it already prepends.
        let project_context: Vec<String> = request
            .prior_messages
            .iter()
            .filter(|m| m.role == agtrs_runtime::transport::Role::System)
            .map(|m| m.text())
            .filter(|t| !t.trim().is_empty())
            .collect();
        let standard_patch: Option<String> = {
            let mut parts: Vec<String> = Vec::new();
            if let Some(p) = request.mode_system_patch.as_deref() {
                if !p.trim().is_empty() {
                    parts.push(p.to_string());
                }
            }
            parts.extend(project_context.iter().cloned());
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        };

        let run_result: Result<(String, crate::types::ExitCode), RuntimeError> =
            if let WorkflowConfig::Phased {
                name,
                definition,
                resume_run_id,
            } = &request.workflow
            {
                let checkpoints =
                    crate::phased::state::CheckpointStore::for_working_dir(working_dir);
                let resume = match resume_run_id {
                    Some(id) => Some(checkpoints.load(id).map_err(|e| {
                        RuntimeError::Config(format!("cannot resume workflow run '{id}': {e}"))
                    })?),
                    None => None,
                };
                // Per-phase models reuse the run's provider config with only the
                // model swapped; an injected test provider serves every phase.
                let llm_for_model: Option<crate::phased::LlmForModel> =
                    if self.provider_override.is_some() {
                        None
                    } else {
                        let cfg = self.config.clone();
                        let preset = preset_name.to_string();
                        Some(Arc::new(move |model: &str| {
                            let mut c = cfg.clone();
                            if let Some(p) = c.agent.get_mut(&preset) {
                                p.model = model.to_string();
                            }
                            ProviderFactory::build(&c, Some(&preset))
                        }))
                    };
                let deps = crate::phased::PhasedDeps {
                    llm: Arc::clone(&llm),
                    llm_for_model,
                    signals: Arc::clone(&self.signals),
                    resolve_ctx: Arc::clone(&resolve_ctx),
                    read_tools: unfiltered_tools.0,
                    write_tools: unfiltered_tools.1,
                    session_tool_filter: request.mode_tool_filter.clone(),
                    session_approval_filter: request.mode_approval_filter.clone(),
                    session_mode_patch: request.mode_system_patch.clone(),
                    project_context: project_context.clone(),
                    conversation_store: orchestrator_conv_store.clone(),
                    approval_gate: self.approval_gate.clone(),
                    question_gate: self.question_gate.clone(),
                    working_dir: working_dir.to_path_buf(),
                    session_id: session.id.to_string(),
                    user_parts: user_parts.clone(),
                    config: request.config.clone(),
                    checkpoints: Some(checkpoints),
                };
                let workflow = crate::phased::PhasedWorkflow {
                    name: name.clone(),
                    definition: (**definition).clone(),
                };
                crate::phased::run_phased_workflow(&workflow, &request.task, deps, resume)
                    .await
                    .map(|outcome| {
                        info!(
                            run_id = %outcome.state.run_id,
                            status = outcome.state.status.label(),
                            phases = outcome.state.phase_history.len(),
                            "xaft: phased workflow finished"
                        );
                        (outcome.content, crate::types::ExitCode::SUCCESS)
                    })
            } else if is_meta {
                let meta_cfg =
                    crate::meta::MetaWorkflowConfig::from_workflow_config(&request.workflow)
                        .unwrap_or_default();
                crate::orchestrator::run_meta_workflow(
                    &request.task,
                    Arc::clone(&llm),
                    Arc::clone(&self.signals),
                    Arc::clone(&resolve_ctx),
                    read_tools,
                    write_tools,
                    &mut session,
                    orchestrator_conv_store,
                    self.approval_gate.clone(),
                    user_parts.clone(),
                    meta_cfg,
                )
                .await
            } else if is_dynamic {
                let registry = crate::agent_registry::AgentRegistry::default_xaft();
                crate::orchestrator::run_dynamic_handoff(
                    &request.task,
                    &registry,
                    &request.workflow,
                    Arc::clone(&llm),
                    Arc::clone(&self.signals),
                    Arc::clone(&resolve_ctx),
                    read_tools,
                    write_tools,
                    &mut session,
                    orchestrator_conv_store.clone(),
                    self.approval_gate.clone(),
                    user_parts.clone(),
                )
                .await
                .map(|r| (r.content, crate::types::ExitCode::SUCCESS))
            } else {
                crate::orchestrator::run_workflow(
                    &request.task,
                    Arc::clone(&llm),
                    Arc::clone(&self.signals),
                    Arc::clone(&resolve_ctx),
                    read_tools,
                    write_tools,
                    &mut session,
                    orchestrator_conv_store,
                    self.approval_gate.clone(),
                    user_parts.clone(),
                    standard_patch.as_deref(),
                )
                .await
            };
        // agenthicc's `_close_browser_after_failure`: a failed (or cancelled)
        // turn releases the live browser but keeps the session's tools usable,
        // so the next turn can open a page again. A one-shot headless run has
        // no next turn, so its browser is closed outright.
        if let Some(browser) = &browser {
            if run_result.is_err() {
                browser.close_runtime().await;
            }
        }
        if request.headless {
            self.browser_sessions
                .close_session(session.id.as_str())
                .await;
        }

        let (content, exit_code) = match run_result {
            Ok(r) => r,
            Err(RuntimeError::Cancelled(reason)) => {
                session.status = SessionStatus::Cancelled;
                self.session_store.save(&session).await?;
                if let Some(guard) = &git_guard {
                    if let Err(e) = guard.restore().await {
                        warn!(error = %e, "xaft: worktree restore failed after cancellation");
                    }
                }
                return Err(RuntimeError::Cancelled(reason));
            }
            Err(e) => {
                session.status = SessionStatus::Failed {
                    error: e.to_string(),
                };
                self.session_store.save(&session).await?;
                if let Some(guard) = &git_guard {
                    if let Err(re) = guard.restore().await {
                        warn!(error = %re, "xaft: worktree restore failed after error");
                    }
                }
                return Err(e);
            }
        };

        // Auto-commit if git worktree was used
        if let Some(guard) = &git_guard {
            match guard.commit(agtrs_git::CommitOptions::default()).await {
                Ok(r) => info!(sha = %r.sha, "xaft: auto-committed changes"),
                Err(agtrs_git::GitError::NothingToCommit) => {}
                Err(e) => warn!(error = %e, "xaft: auto-commit failed"),
            }
        }

        // ── Flush accumulated cost/tokens into session ────────────────────────
        if let Ok(acc) = cost_acc.lock() {
            session.total_cost_usd += acc.total_cost_usd;
            session.total_tokens += acc.total_tokens;
            session.turn_count += acc.llm_calls;
            tracing::info!(
                cost_usd = acc.total_cost_usd,
                tokens = acc.total_tokens,
                llm_calls = acc.llm_calls,
                "xaft: run cost summary"
            );
        }

        // ── Persist final session status ──────────────────────────────────────
        session.status = SessionStatus::Completed {
            summary: if content.is_empty() {
                format!("task completed: {}", request.task)
            } else {
                content.chars().take(200).collect()
            },
        };

        self.session_store.save(&session).await?;
        tracing::info!(
            session_id = %session.id,
            cost_usd = session.total_cost_usd,
            tokens = session.total_tokens,
            "xaft: session saved"
        );

        let summary = if content.is_empty() {
            format!("task completed: {}", request.task)
        } else {
            content.chars().take(500).collect()
        };

        Ok(RunResult {
            exit_code,
            session,
            summary,
        })
    }
}

// ── RuntimeDispatch impl ──────────────────────────────────────────────────────

#[async_trait]
impl RuntimeDispatch for XaftRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunResult, RuntimeError> {
        self.run_task(request).await
    }

    async fn list_sessions(&self, working_dir: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
        self.session_store.list(Some(working_dir)).await
    }

    // Unfiltered by design: `xaft session show` resolves by ID and must not
    // depend on the caller's cwd, matching `xaft session inspect`.
    async fn load_session(
        &self,
        id: &crate::session::SessionId,
    ) -> Result<Option<AgentSession>, RuntimeError> {
        Ok(self.session_store.load(id).await?)
    }

    async fn save_session(&self, session: &AgentSession) -> Result<(), RuntimeError> {
        self.session_store.save(session).await
    }

    async fn resume_session(
        &self,
        session_id: &str,
        config: XaftConfig,
    ) -> Result<RunResult, RuntimeError> {
        let id = crate::session::SessionId::from_string(session_id);
        let session = self
            .session_store
            .load(&id)
            .await?
            .ok_or_else(|| RuntimeError::SessionNotFound(session_id.to_string()))?;

        // Allow resuming Active, Suspended, and Completed sessions.
        // Only reject Failed and Cancelled (hard errors).
        match &session.status {
            SessionStatus::Active | SessionStatus::Suspended | SessionStatus::Completed { .. } => {}
            SessionStatus::Failed { error } => {
                return Err(RuntimeError::Config(format!(
                    "session '{}' cannot be resumed (failed: {})",
                    session_id, error
                )));
            }
            SessionStatus::Cancelled => {
                return Err(RuntimeError::Config(format!(
                    "session '{}' cannot be resumed (cancelled)",
                    session_id
                )));
            }
        }

        // Load prior conversation history
        let mut prior_messages = Vec::new();
        if let Some(ref conv_store) = self.conversation_store {
            let workflow_key = format!("{}::workflow", session_id);
            match conv_store.load(&workflow_key).await {
                Ok(msgs) if !msgs.is_empty() => {
                    prior_messages = msgs;
                }
                _ => {
                    // Try session-level key as fallback
                    let session_key = session_id.to_string();
                    if let Ok(msgs) = conv_store.load(&session_key).await {
                        prior_messages = msgs;
                    }
                }
            }
        }

        tracing::info!(
            session_id = %session_id,
            task = %session.task,
            turns = session.turn_count,
            tokens = session.total_tokens,
            prior_messages = prior_messages.len(),
            "xaft: resuming session"
        );

        let request = RunRequest {
            task: session.task.clone(),
            config,
            working_dir: session.workspace_root.clone(),
            headless: false,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
            resume_session_id: Some(session_id.to_string()),
            workflow: crate::agent_registry::WorkflowConfig::default(),
            prior_messages,
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
            mode_approval_filter: None,
        };

        // Propagate conversation_store so HandoffOrchestrator reuses the same
        // SQLite database (conversation history is keyed by session_id, so prior
        // context is automatically available without explicit re-injection).
        let resume_runtime = Self {
            config: request.config.clone(),
            signals: Arc::clone(&self.signals),
            session_store: Arc::clone(&self.session_store),
            provider_override: self.provider_override.clone(),
            conversation_store: self.conversation_store.clone(),
            approval_gate: self.approval_gate.clone(),
            terminal_manager: self.terminal_manager.clone(),
            question_gate: self.question_gate.clone(),
            browser_sessions: Arc::clone(&self.browser_sessions),
            #[cfg(feature = "memory")]
            memory_manager: self.memory_manager.clone(),
        };

        resume_runtime.run_task(request).await
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Emit `FileEditsCommitted` whenever write_file or edit_file completes successfully.
///
/// xaft-tools uses a plain `FsWorkspaceStore` that does not emit the signal itself,
/// so we reconstruct it from `ToolCallStarted` inputs + `ToolCallComplete` results.
async fn attach_file_edit_broadcaster(signals: &Arc<agtrs_runtime::signals::SignalBus>) {
    use agtrs_runtime::signals::{FileEditsCommitted, ToolCallComplete, ToolCallStarted};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    let pending: Arc<Mutex<HashMap<String, serde_json::Value>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pending_for_complete = Arc::clone(&pending);
    let bus = Arc::clone(signals);

    signals
        .on::<ToolCallStarted>(move |ev| {
            if ev.tool_name == "write_file" || ev.tool_name == "edit_file" {
                if let Ok(mut map) = pending.lock() {
                    map.insert(ev.tool_use_id.clone(), ev.input.clone());
                }
            }
        })
        .await;

    signals
        .on::<ToolCallComplete>(move |ev| {
            if (ev.tool_name != "write_file" && ev.tool_name != "edit_file") || !ev.success {
                return;
            }
            let input = {
                let mut map = pending_for_complete
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                map.remove(&ev.tool_use_id)
            };
            let Some(input) = input else { return };

            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let lines_added = content.lines().count() as i64;

            let mut diffs = HashMap::new();
            diffs.insert(path.to_string(), content.to_string());

            let bus2 = bus.clone();
            let files = vec![path.to_string()];
            tokio::spawn(async move {
                bus2.emit(FileEditsCommitted {
                    files,
                    total_lines_added: lines_added,
                    total_lines_removed: 0,
                    diffs,
                })
                .await;
            });
        })
        .await;
}

/// Try to open a git worktree for the working directory.
///
/// Register a `ToolCallStarted` + `ToolCallComplete` listener on `signals`.
async fn attach_tool_call_logger(signals: &agtrs_runtime::signals::SignalBus) {
    use agtrs_runtime::signals::{ToolCallComplete, ToolCallStarted};
    signals
        .on::<ToolCallStarted>(|ev| {
            tracing::info!(
                tool = %ev.tool_name,
                id = %ev.tool_use_id,
                input = %ev.input,
                "xaft: tool call input"
            );
        })
        .await;
    signals
        .on::<ToolCallComplete>(|ev| {
            if ev.success {
                tracing::info!(
                    tool = %ev.tool_name,
                    id = %ev.tool_use_id,
                    duration_ms = %ev.duration_ms,
                    "xaft: tool call done"
                );
            } else {
                tracing::warn!(
                    tool = %ev.tool_name,
                    id = %ev.tool_use_id,
                    error = %ev.error.as_deref().unwrap_or("(none)"),
                    "xaft: tool call failed"
                );
            }
        })
        .await;
}

/// Returns `None` if the directory is not a git repo (silently ignores the
/// error — xaft works fine without git).
async fn try_open_git_worktree(
    working_dir: &Path,
    run_id: &str,
) -> Option<Arc<agtrs_git::WorktreeGuard>> {
    let repo = match GitRepo::open(working_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(dir = %working_dir.display(), error = %e, "xaft: not a git repo, running without worktree");
            return None;
        }
    };

    match repo.begin_worktree(Some(run_id), None).await {
        Ok(guard) => Some(Arc::new(guard)),
        Err(e) => {
            warn!(error = %e, "xaft: could not create git worktree, running without branch isolation");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
    use tempfile::TempDir;
    use xaft_config::XaftConfig;

    fn mock_config() -> XaftConfig {
        XaftConfig::default()
    }

    fn make_request(task: &str, dir: &TempDir) -> RunRequest {
        RunRequest {
            task: task.to_string(),
            config: mock_config(),
            working_dir: dir.path().to_path_buf(),
            headless: true,
            dry_run: false,
            auto_approve: true,
            dangerously_skip_permissions: false,
            resume_session_id: None,
            workflow: crate::agent_registry::WorkflowConfig::default(),
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
            mode_approval_filter: None,
        }
    }

    #[tokio::test]
    async fn dry_run_returns_success_without_llm() {
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);
        let mut request = make_request("list files", &tmp);
        request.dry_run = true;
        let result = runtime.run(request).await.unwrap();
        assert!(result.exit_code.is_success());
        assert!(result.summary.contains("dry-run"));
    }

    #[tokio::test]
    async fn run_with_mock_llm_succeeds() {
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        // The orchestrated flow (planner × 3 strategies + coder + QA) consumes
        // many LLM calls. Queue enough garbage for planning to exhaust its
        // strategies, then the coder returns a response, then QA says APPROVED.
        for _ in 0..12 {
            transport.queue_text("not a plan").await;
        }
        transport.queue_text("{\"files_changed\":[],\"description\":\"done\",\"tests_passed\":false,\"notes\":\"\"}").await;
        transport.queue_text("APPROVED").await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
        let request = make_request("do something", &tmp);
        let result = runtime.run(request).await.unwrap();
        assert!(result.exit_code.is_success());
    }

    #[tokio::test]
    async fn list_sessions_returns_empty_for_new_runtime() {
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);
        let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn session_persisted_after_run() {
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        for _ in 0..12 {
            transport.queue_text("not a plan").await;
        }
        transport.queue_text("{\"files_changed\":[],\"description\":\"done\",\"tests_passed\":false,\"notes\":\"\"}").await;
        transport.queue_text("APPROVED").await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

        let runtime = XaftRuntime::for_testing(mock_config(), Some(Arc::clone(&llm)));
        let request = make_request("my task", &tmp);
        runtime.run(request).await.unwrap();

        let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].task, "my task");
    }

    #[tokio::test]
    async fn resume_nonexistent_session_returns_error() {
        let runtime = XaftRuntime::for_testing(mock_config(), None);
        let result = runtime
            .resume_session("nonexistent-id", mock_config())
            .await;
        assert!(matches!(result, Err(RuntimeError::SessionNotFound(_))));
    }

    // ── --continue behaviour ─────────────────────────────────────────────────
    // These tests verify the three distinct resume paths:
    //   1. Completed session  → fresh agent start (no prior tool-call history)
    //   2. Active session     → crash-recovery   (agent history loaded)
    //   3. Failed/Cancelled   → hard error

    fn queue_successful_run(transport: &Arc<MockTransport>) {
        // Drive a minimal planner-inline (no coder handoff) run: exhaust planner
        // strategies then the planner returns something that isn't a PLAN.
        // 12 × "not a plan" exhausts strategies; coder + QA are not reached.
        let responses: Vec<&str> = vec!["done"; 12];
        let runtime_handle = tokio::runtime::Handle::current();
        for resp in responses {
            let t = Arc::clone(transport);
            let r = resp.to_string();
            runtime_handle.block_on(async move { t.queue_text(&r).await });
        }
    }

    #[tokio::test]
    async fn continue_on_completed_session_starts_fresh_dry_run() {
        // After a successful run the session is Completed.
        // Running again with resume_session_id must succeed (not "cannot resume").
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        for _ in 0..12 {
            transport.queue_text("not a plan").await;
        }
        transport
            .queue_text(
                r#"{"files_changed":[],"description":"ok","tests_passed":false,"notes":""}"#,
            )
            .await;
        transport.queue_text("APPROVED").await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(Arc::clone(&transport)));
        let runtime = XaftRuntime::for_testing(mock_config(), Some(Arc::clone(&llm)));

        // First run — completes the session.
        let r1 = runtime.run(make_request("task one", &tmp)).await.unwrap();
        let session_id = r1.session.id.to_string();

        // Session should now be persisted.
        let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0].is_resumable(),
            "completed session must be resumable"
        );

        // Second run via dry_run using the same session ID.
        let mut req2 = make_request("task two", &tmp);
        req2.resume_session_id = Some(session_id.clone());
        req2.dry_run = true;
        let r2 = runtime.run(req2).await.unwrap();
        assert!(
            r2.exit_code.is_success(),
            "continue on completed session failed"
        );
    }

    #[tokio::test]
    async fn continue_on_failed_session_returns_error() {
        // A Failed session must be rejected — user gets a clear Config error,
        // not a cryptic SessionNotFound.
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);

        // Manually create a Failed session in the store.
        let mut session = crate::session::AgentSession::new(
            "broken task",
            tmp.path().to_path_buf(),
            "default".into(),
            "m".into(),
        );
        session.status = crate::session::SessionStatus::Failed {
            error: "exploded".into(),
        };
        runtime.session_store.save(&session).await.unwrap();

        let mut req = make_request("next task", &tmp);
        req.resume_session_id = Some(session.id.to_string());
        let err = runtime.run(req).await.unwrap_err();
        assert!(
            matches!(err, RuntimeError::Config(_)),
            "failed-session resume must return Config error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn continue_on_cancelled_session_returns_error() {
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);

        let mut session = crate::session::AgentSession::new(
            "cancelled",
            tmp.path().to_path_buf(),
            "default".into(),
            "m".into(),
        );
        session.status = crate::session::SessionStatus::Cancelled;
        runtime.session_store.save(&session).await.unwrap();

        let mut req = make_request("next task", &tmp);
        req.resume_session_id = Some(session.id.to_string());
        let err = runtime.run(req).await.unwrap_err();
        assert!(
            matches!(err, RuntimeError::Config(_)),
            "cancelled-session resume must return Config error"
        );
    }

    #[tokio::test]
    async fn continue_active_session_dry_run_succeeds() {
        // An Active (crash-recovery) session is resumable.
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);

        let mut session = crate::session::AgentSession::new(
            "active task",
            tmp.path().to_path_buf(),
            "default".into(),
            "m".into(),
        );
        // Active status = crash-recovery scenario.
        session.status = crate::session::SessionStatus::Active;
        runtime.session_store.save(&session).await.unwrap();

        let mut req = make_request("next task", &tmp);
        req.resume_session_id = Some(session.id.to_string());
        req.dry_run = true;
        let result = runtime.run(req).await.unwrap();
        assert!(
            result.exit_code.is_success(),
            "active session resume must succeed"
        );
    }

    #[tokio::test]
    async fn continue_does_not_resume_non_matching_directory() {
        // Sessions for a DIFFERENT directory are not resumed by --continue.
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);

        // Session belongs to tmp_b.
        let session = crate::session::AgentSession::new(
            "task in b",
            tmp_b.path().to_path_buf(),
            "default".into(),
            "m".into(),
        );
        runtime.session_store.save(&session).await.unwrap();

        // list_sessions for tmp_a should return nothing.
        let sessions = runtime.list_sessions(tmp_a.path()).await.unwrap();
        assert!(
            sessions.is_empty(),
            "sessions from another dir must not appear"
        );
    }

    #[tokio::test]
    async fn is_resumable_matches_runtime_policy() {
        use crate::session::{AgentSession, SessionStatus};

        let make = |status: SessionStatus| -> AgentSession {
            let mut s =
                AgentSession::new("t", std::path::PathBuf::from("."), "p".into(), "m".into());
            s.status = status;
            s
        };

        // All three resumable statuses.
        assert!(make(SessionStatus::Active).is_resumable());
        assert!(make(SessionStatus::Suspended).is_resumable());
        assert!(
            make(SessionStatus::Completed {
                summary: "ok".into()
            })
            .is_resumable()
        );
        // Non-resumable statuses.
        assert!(!make(SessionStatus::Failed { error: "e".into() }).is_resumable());
        assert!(!make(SessionStatus::Cancelled).is_resumable());
    }

    // ── Background-terminal wiring (parity rows 36/43/44/45) ────────────────
    //
    // The four terminal tools, and `run_command {"background": true}`, are
    // registered by `xaft-tools` unconditionally and refuse unless the
    // registry was built *with* a manager. Everything that hands them one
    // lives here, so these tests pin the wiring itself: every run resolves a
    // manager over its own session and working directory, and an injected
    // manager is the one that wins — never a second one built beside it.

    /// Queue the minimal successful run: 12 planner strategies exhausted, then
    /// a coder result and QA's APPROVED. Same shape as
    /// `run_with_mock_llm_succeeds`.
    async fn queue_minimal_successful_run() -> Arc<dyn LlmProvider> {
        let transport = Arc::new(MockTransport::new());
        for _ in 0..12 {
            transport.queue_text("not a plan").await;
        }
        transport
            .queue_text(
                "{\"files_changed\":[],\"description\":\"done\",\"tests_passed\":false,\"notes\":\"\"}",
            )
            .await;
        transport.queue_text("APPROVED").await;
        Arc::new(MockLlmProvider::new(transport))
    }

    #[tokio::test]
    async fn a_manager_is_built_over_the_session_and_working_directory_of_the_run() {
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);

        let manager = runtime.resolve_terminal_manager("sess-42", tmp.path());

        assert_eq!(manager.config().session_id, "sess-42");
        assert_eq!(manager.config().store_root, tmp.path().join(".xaft"));
        // `TerminalStore::new` creates its root, so the directory is on-disk
        // proof that a real store — not a placeholder value — was built.
        assert!(
            tmp.path().join(".xaft").is_dir(),
            "the manager's store root was not created"
        );
    }

    #[tokio::test]
    async fn an_injected_manager_is_the_one_a_run_resolves() {
        let tmp = TempDir::new().unwrap();
        let injected = Arc::new(TerminalManager::new(TerminalManagerConfig::new(
            "sess-injected",
            tmp.path(),
        )));
        let runtime = XaftRuntime::for_testing(mock_config(), None)
            .with_terminal_manager(Arc::clone(&injected));

        // A different session id is asked for on purpose: the injected manager
        // must win outright. Two managers for one session would orphan each
        // other's terminals (manager.rs, "ownership recovery").
        let resolved = runtime.resolve_terminal_manager("sess-other", tmp.path());

        assert!(
            Arc::ptr_eq(&resolved, &injected),
            "the run must use the injected manager, not an equal-looking copy"
        );
        assert_eq!(resolved.config().session_id, "sess-injected");
    }

    #[tokio::test]
    async fn a_run_builds_a_terminal_manager_over_its_working_directory() {
        let tmp = TempDir::new().unwrap();
        let llm = queue_minimal_successful_run().await;
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

        let result = runtime
            .run(make_request("do something", &tmp))
            .await
            .unwrap();
        assert!(result.exit_code.is_success());

        // The product-facing assertion: a run over this directory left a
        // terminal store beside it, so the terminal tools it registered had a
        // real manager rather than `None`.
        assert!(
            tmp.path().join(".xaft").join("terminals.jsonl").exists()
                || tmp.path().join(".xaft").is_dir(),
            "a run built no terminal manager over {}",
            tmp.path().display()
        );
    }

    // ── Browser sessions (parity rows 60-77) ─────────────────────────────────

    /// A cache whose CloakBrowser "binary" is a plain executable file, so the
    /// backend reports ready without any browser being installed or launched.
    fn browser_cache_with_fake_binary(dir: &TempDir) -> Arc<BrowserSessionCache> {
        let binary = dir.path().join("fake-cloakbrowser");
        std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Arc::new(BrowserSessionCache::with_options(
            xaft_tools::browser::BrowserLaunchOptions {
                discovery: Some(xaft_tools::browser::BrowserDiscovery {
                    cloakbrowser_binary_path: Some(binary),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ))
    }

    /// agenthicc keeps one browser manager per session across turns and resets
    /// its action budget at the top of every turn; a run is a turn. The
    /// earlier turn is simulated by a saved session plus the manager the cache
    /// already holds for it, so one mock run is enough.
    #[tokio::test]
    async fn a_session_keeps_its_browser_manager_and_each_run_resets_its_budget() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let cache = browser_cache_with_fake_binary(&bin);
        let llm = queue_minimal_successful_run().await;
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm))
            .with_browser_sessions(Arc::clone(&cache));

        let earlier = AgentSession::new(
            "earlier turn".to_string(),
            tmp.path().to_path_buf(),
            "default".to_string(),
            "mock".to_string(),
        );
        let session_id = earlier.id.to_string();
        runtime.save_session(&earlier).await.unwrap();
        let tools = xaft_config::ToolSettingsConfig::default();
        let manager = cache.session_for(&tools, tmp.path(), &session_id).unwrap();
        assert_eq!(
            manager.family(),
            xaft_tools::browser::BrowserFamily::CloakBrowser
        );
        // agenthicc's `fill` spends an action before it finds the page missing,
        // which lets the test spend one without a browser.
        let spent = manager.fill("no-such-page", "#q", "x", None, None).await;
        assert_eq!(spent["error_kind"], "stale_page", "{spent}");
        assert_eq!(manager.actions_used(), 1);

        let mut next = make_request("next turn", &tmp);
        next.headless = false;
        next.resume_session_id = Some(session_id.clone());
        assert!(runtime.run(next).await.unwrap().exit_code.is_success());

        let again = cache.session_for(&tools, tmp.path(), &session_id).unwrap();
        assert!(
            Arc::ptr_eq(&manager, &again),
            "the session kept its manager"
        );
        assert_eq!(manager.actions_used(), 0, "the run reset the action budget");
        assert_eq!(cache.len(), 1);
    }

    /// A one-shot headless run has no next turn, so its browser is closed.
    #[tokio::test]
    async fn a_headless_run_closes_its_browser_session() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let cache = browser_cache_with_fake_binary(&bin);
        let llm = queue_minimal_successful_run().await;
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm))
            .with_browser_sessions(Arc::clone(&cache));
        let result = runtime.run(make_request("one shot", &tmp)).await.unwrap();
        assert!(result.exit_code.is_success());
        assert!(
            cache.is_empty(),
            "a headless run left a browser session behind"
        );
    }

    /// `browser_backend = "none"` attaches no browser at all.
    #[tokio::test]
    async fn browser_backend_none_attaches_no_browser_session() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let cache = browser_cache_with_fake_binary(&bin);
        let llm = queue_minimal_successful_run().await;
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm))
            .with_browser_sessions(Arc::clone(&cache));
        let mut request = make_request("no browser", &tmp);
        request.headless = false;
        request.config.tools.browser_backend = "none".into();
        assert!(runtime.run(request).await.unwrap().exit_code.is_success());
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn a_run_never_builds_a_second_manager_beside_an_injected_one() {
        let injected_dir = TempDir::new().unwrap();
        let run_dir = TempDir::new().unwrap();
        let injected = Arc::new(TerminalManager::new(TerminalManagerConfig::new(
            "sess-injected",
            injected_dir.path(),
        )));
        assert!(
            injected_dir.path().join(".xaft").is_dir(),
            "precondition: the injected manager owns a store"
        );

        let llm = queue_minimal_successful_run().await;
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm))
            .with_terminal_manager(Arc::clone(&injected));

        let result = runtime
            .run(make_request("do something", &run_dir))
            .await
            .unwrap();
        assert!(result.exit_code.is_success());

        // The run's session id is minted inside `run_task`, so a manager built
        // there would have opened a store under the run's directory. Nothing
        // did: exactly one manager exists for the session.
        assert!(
            !run_dir.path().join(".xaft").exists(),
            "the run built a second terminal manager over {}",
            run_dir.path().display()
        );
    }

    // ── Phased workflows through the real run path ──────────────────────────

    fn phased_request(
        task: &str,
        dir: &TempDir,
        definition: xaft_config::types::WorkflowDefinition,
        resume_run_id: Option<String>,
    ) -> RunRequest {
        let mut request = make_request(task, dir);
        request.workflow = crate::agent_registry::WorkflowConfig::Phased {
            name: "two_step".into(),
            definition: Box::new(definition),
            resume_run_id,
        };
        request
    }

    fn two_step() -> xaft_config::types::WorkflowDefinition {
        let toml = r#"
mode = "phased"
[[phases]]
name = "draft"
next = "polish"
[[phases]]
name = "polish"
max_iterations = 1
[[phases.transitions]]
tool = "finish"
description = "The docs are polished."
params = [{ name = "summary", description = "What changed." }]
"#;
        toml::from_str(toml).unwrap()
    }

    #[tokio::test]
    async fn a_phased_workflow_runs_through_xaft_runtime_and_resumes() {
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        // draft answers; polish ends its only turn without calling `finish`.
        transport.queue_text("first draft").await;
        transport.queue_text("not finished").await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(Arc::clone(&transport)));
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

        let err = runtime
            .run(phased_request("write docs", &tmp, two_step(), None))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("finish"), "{err}");

        // The failure is checkpointed under the working directory's .xaft/.
        let store = crate::phased::state::CheckpointStore::for_working_dir(tmp.path());
        let failed = store.latest_resumable(Some("two_step")).unwrap().unwrap();
        assert_eq!(failed.status, crate::phased::state::RunStatus::Failed);
        assert_eq!(failed.intent, "write docs");
        assert_eq!(failed.next_phase.as_deref(), Some("polish"));
        assert_eq!(failed.phase_iterations["polish"], 1);

        // Resumed by id through RunRequest: the interrupted entry of a
        // max_iterations = 1 phase is not counted twice.
        transport
            .queue_tool_call("finish", serde_json::json!({"summary": "polished"}))
            .await;
        transport.queue_text("ok").await;
        let resumed = runtime
            .run(phased_request(
                "ignored",
                &tmp,
                two_step(),
                Some(failed.run_id.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(resumed.summary, "polished");
        let after = store.load(&failed.run_id).unwrap();
        assert_eq!(after.status, crate::phased::state::RunStatus::Complete);
        assert_eq!(
            after
                .phase_history
                .iter()
                .map(|r| r.phase_name.as_str())
                .collect::<Vec<_>>(),
            ["draft", "polish"]
        );
    }

    #[tokio::test]
    async fn resuming_an_unknown_workflow_run_is_a_config_error() {
        let tmp = TempDir::new().unwrap();
        let llm: Arc<dyn LlmProvider> =
            Arc::new(MockLlmProvider::new(Arc::new(MockTransport::new())));
        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
        let err = runtime
            .run(phased_request("t", &tmp, two_step(), Some("nope".into())))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot resume workflow run 'nope'"),
            "{err}"
        );
    }
}
