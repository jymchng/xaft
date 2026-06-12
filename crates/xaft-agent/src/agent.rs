//! `XaftAgent` — production-grade coding agent implementing the agtrs `Agent` trait.
//!
//! # Features
//!
//! - **Role-aware system prompts** — `AgentRole` selects a curated default prompt.
//! - **Git auto-commit** — calls `WorktreeGuard::commit()` on successful finish.
//! - **Streaming emission** — forwards `XaftLlmCallStarting` signals on each LLM call.
//! - **Stream sink** — optional `StreamSink` for forwarding events to websocket / SSE.
//!
//! # Usage
//!
//! ```rust,ignore
//! use xaft_agent::builder::AgentBuilder;
//! use xaft_agent::config::AgentRole;
//!
//! let agent = AgentBuilder::new("my-coder")
//!     .role(AgentRole::Coder)
//!     .tools(tools)
//!     .with_git_guard(guard)
//!     .build();
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tracing::{instrument, warn};

use agtrs_git::{CommitOptions, GitError, WorktreeGuard};
use agtrs_runtime::agent::{Agent, AgentConfig, AgentContext, AgentResponse};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmOptions;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::streaming::StreamEvent;
use agtrs_runtime::tool::ErasedTool;
use agtrs_runtime::transport::{Message, StopReason, TokenUsage};

use crate::config::{CommitPolicy, XaftAgentConfig};
use crate::prompts::build_system_prompt;
use crate::signals::{XaftAgentOutput, XaftCommitCreated, XaftLlmCallStarting};
use crate::stream::StreamSink;

// ── XaftAgent ─────────────────────────────────────────────────────────────────

/// A production-grade coding agent wrapping the agtrs `Agent` trait.
///
/// See module-level docs for usage.
pub struct XaftAgent {
    /// Unique agent name.
    name: String,
    /// agtrs runtime config (derived from `XaftAgentConfig`).
    pub(crate) config: AgentConfig,
    /// Tool list.
    pub(crate) tools: Vec<Arc<ErasedTool>>,
    /// Optional git worktree guard for auto-commit.
    pub(crate) git_guard: Option<Arc<WorktreeGuard>>,
    /// When to auto-commit.
    pub(crate) commit_policy: CommitPolicy,
    /// Optional stream sink for forwarding events.
    pub(crate) stream_sink: Option<Arc<dyn StreamSink>>,
    /// Optional shared signal bus.
    pub(crate) signals: Option<Arc<SignalBus>>,
    /// LLM call counter (per-run, reset is caller's responsibility via fresh agent).
    call_index: AtomicUsize,
}

impl std::fmt::Debug for XaftAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XaftAgent")
            .field("name", &self.name)
            .field("commit_policy", &self.commit_policy)
            .field("tools_count", &self.tools.len())
            .finish()
    }
}

impl XaftAgent {
    /// Create from xaft-level config.
    pub(crate) fn from_xaft_config(
        name: impl Into<String>,
        xaft: XaftAgentConfig,
        extra_prompt: Option<&str>,
        tools: Vec<Arc<ErasedTool>>,
        git_guard: Option<Arc<WorktreeGuard>>,
        stream_sink: Option<Arc<dyn StreamSink>>,
        signals: Option<Arc<SignalBus>>,
    ) -> Self {
        let system_prompt = build_system_prompt(&xaft.role, extra_prompt);
        let config = AgentConfig {
            system_prompt,
            max_turns: xaft.max_turns,
            parallel_tool_policy: xaft.parallel_tool_policy,
            max_concurrent_tools: xaft.max_concurrent_tools,
            parallel_tool_calls: false, // use parallel_tool_policy instead
            deadline: xaft.deadline,
            max_cost_usd: xaft.cost_limit_usd,
            max_tokens_per_turn: xaft.max_tokens_per_turn,
            temperature: xaft.temperature,
            strict_capability_check: false,
            ..Default::default()
        };
        Self {
            name: name.into(),
            config,
            tools,
            git_guard,
            commit_policy: xaft.commit_policy,
            stream_sink,
            signals,
            call_index: AtomicUsize::new(0),
        }
    }

    /// Construct directly from an `AgentConfig` (for tests and internal use).
    pub fn new_with_config(
        name: impl Into<String>,
        config: AgentConfig,
        tools: Vec<Arc<ErasedTool>>,
    ) -> Self {
        Self {
            name: name.into(),
            config,
            tools,
            git_guard: None,
            commit_policy: CommitPolicy::Never,
            stream_sink: None,
            signals: None,
            call_index: AtomicUsize::new(0),
        }
    }

    /// Return the agent's commit policy.
    pub fn commit_policy(&self) -> &CommitPolicy {
        &self.commit_policy
    }

    /// Emit a stream event if a sink is attached.
    fn emit_stream(&self, event: StreamEvent) {
        if let Some(sink) = &self.stream_sink {
            sink.send(event);
        }
    }

    /// Emit a signal on the attached signal bus if present.
    fn try_emit_signal<S: Clone + Send + Sync + 'static>(&self, signal: S) {
        if let Some(bus) = &self.signals {
            let bus = Arc::clone(bus);
            tokio::spawn(async move {
                bus.emit(signal).await;
            });
        }
    }

    /// Auto-commit if the policy and stop reason call for it.
    #[instrument(name = "xaft_auto_commit", skip_all, fields(agent = %self.name))]
    async fn maybe_auto_commit(&self, stop_reason: &StopReason) {
        let guard = match &self.git_guard {
            Some(g) => g,
            None => return,
        };

        let should_commit = match &self.commit_policy {
            CommitPolicy::Never => false,
            CommitPolicy::OnSuccess => matches!(stop_reason, StopReason::EndTurn),
            CommitPolicy::Always => true,
        };

        if !should_commit {
            return;
        }

        match guard.commit(CommitOptions::default()).await {
            Ok(receipt) => {
                let n_files = receipt.files_committed.len();
                tracing::info!(
                    agent = %self.name,
                    sha = %receipt.sha,
                    files = n_files,
                    "xaft: git auto-commit"
                );
                let short_sha = receipt.sha.chars().take(8).collect::<String>();
                self.try_emit_signal(XaftCommitCreated {
                    agent_name: self.name.clone(),
                    short_sha,
                    message: receipt.message.clone(),
                    files_changed: n_files,
                    lines_added: receipt.lines_added,
                    lines_removed: receipt.lines_removed,
                });
            }
            Err(GitError::NothingToCommit) => {
                tracing::debug!(agent = %self.name, "xaft: nothing to commit");
            }
            Err(GitError::AlreadyCommitted { .. }) | Err(GitError::AlreadyRestored { .. }) => {
                tracing::debug!(agent = %self.name, "xaft: worktree already committed/restored");
            }
            Err(e) => {
                warn!(agent = %self.name, error = %e, "xaft: git auto-commit failed");
            }
        }
    }
}

#[async_trait]
impl Agent for XaftAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn system_prompt(&self) -> String {
        self.config.system_prompt.clone()
    }

    fn tools(&self) -> Vec<Arc<ErasedTool>> {
        self.tools.clone()
    }

    fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Inject any per-run initialization into the context before the first LLM call.
    async fn on_start(&self, ctx: &mut AgentContext) -> Result<(), AgtrsError> {
        tracing::debug!(agent = %self.name, "xaft: on_start");
        // Record the agent role in context state for downstream inspection
        ctx.set_state_entry(
            "xaft_agent_name",
            serde_json::Value::String(self.name.clone()),
        );
        Ok(())
    }

    /// Emit `XaftLlmCallStarting` before each LLM call.
    ///
    /// Also forwards a `StreamEvent::TextDelta` with a zero-width placeholder
    /// so streaming consumers know a call is starting.
    async fn before_llm_call(
        &self,
        _messages: &mut Vec<Message>,
        _options: &mut LlmOptions,
    ) -> Result<(), AgtrsError> {
        let idx = self.call_index.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(agent = %self.name, call_index = idx, "xaft: before_llm_call");

        self.try_emit_signal(XaftLlmCallStarting {
            agent_name: self.name.clone(),
            call_index: idx,
        });

        Ok(())
    }

    /// Forward tool results to the stream sink.
    async fn on_tool_result(
        &self,
        result: &agtrs_runtime::tool::ToolResult,
        _ctx: &AgentContext,
    ) -> Result<(), AgtrsError> {
        self.emit_stream(StreamEvent::ToolResult {
            result: result.clone(),
        });
        Ok(())
    }

    /// Log per-turn usage.
    async fn on_turn_complete(
        &self,
        usage: &agtrs_runtime::transport::TokenUsage,
        _ctx: &AgentContext,
    ) -> Result<(), AgtrsError> {
        tracing::debug!(
            agent = %self.name,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            "xaft: turn complete"
        );
        Ok(())
    }

    /// Emit a `Done` event and optionally auto-commit.
    async fn on_finish(
        &self,
        response: &AgentResponse,
        _ctx: &AgentContext,
    ) -> Result<(), AgtrsError> {
        tracing::debug!(
            agent = %self.name,
            turns = response.turns,
            stop_reason = ?response.stop_reason,
            "xaft: on_finish"
        );

        self.emit_stream(StreamEvent::Done {
            content: response.content.clone(),
            stop_reason: response.stop_reason.clone(),
            usage: TokenUsage::new(
                response.total_usage.input_tokens,
                response.total_usage.output_tokens,
            ),
            turns: response.turns,
            agent_name: self.name.clone(),
            messages: vec![], // full message history not re-emitted here
        });

        // Emit XaftAgentOutput so TUI subscribers see the final text without streaming.
        if !response.content.is_empty() {
            self.try_emit_signal(XaftAgentOutput {
                agent_name: self.name.clone(),
                content: response.content.clone(),
            });
        }

        self.maybe_auto_commit(&response.stop_reason).await;

        Ok(())
    }
}
