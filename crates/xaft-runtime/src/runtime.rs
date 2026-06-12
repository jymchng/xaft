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
use xaft_tools::registry::ToolRegistryBuilder;

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
            let loader = SkillLoader::for_working_dir(working_dir);
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
        let read_tools = {
            ToolRegistryBuilder::new(working_dir)
                .without_git()
                .build_reader()
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?
                .all()
        };

        let write_tools = {
            let mut builder = ToolRegistryBuilder::new(working_dir).with_shell();
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

        // ── Resolve context (for planner tool-call strategy) ──────────────────
        let resolve_ctx = Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
            injectable_runtime::EmptySingletonStore,
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

        let run_result: Result<(String, crate::types::ExitCode), RuntimeError> = if is_meta {
            let meta_cfg = crate::meta::MetaWorkflowConfig::from_workflow_config(&request.workflow)
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
            )
            .await
        };

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
}
