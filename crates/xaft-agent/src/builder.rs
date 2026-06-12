//! Fluent builders for `XaftAgent` and `PlanModeAgent`.
//!
//! # Examples
//!
//! ## `XaftAgent`
//!
//! ```rust,ignore
//! use xaft_agent::builder::AgentBuilder;
//! use xaft_agent::config::AgentRole;
//!
//! let agent = AgentBuilder::new("coder")
//!     .role(AgentRole::Coder)
//!     .system_prompt_extra("Always use Rust 2024 edition.")
//!     .tools(tool_registry.all())
//!     .with_git_guard(guard)
//!     .commit_on_success()
//!     .max_turns(30)
//!     .build();
//! ```
//!
//! ## `PlanModeAgent`
//!
//! ```rust,ignore
//! use xaft_agent::builder::PlanAgentBuilder;
//!
//! let agent = PlanAgentBuilder::new("plan-coder")
//!     .role(AgentRole::Coder)
//!     .tools(tools)
//!     .max_refinement_iterations(3)
//!     .build();
//! ```

use std::sync::Arc;
use std::time::Duration;

use agtrs_git::WorktreeGuard;
use agtrs_runtime::agent::AgentConfig;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::ErasedTool;

use crate::agent::XaftAgent;
use crate::config::{AgentRole, CommitPolicy, EscalationPolicy, PlanModeConfig, XaftAgentConfig};
use crate::plan_mode::PlanModeAgent;
use crate::stream::StreamSink;

// ── AgentBuilder ──────────────────────────────────────────────────────────────

/// Fluent builder for [`XaftAgent`].
pub struct AgentBuilder {
    name: String,
    xaft_config: XaftAgentConfig,
    extra_prompt: Option<String>,
    tools: Vec<Arc<ErasedTool>>,
    git_guard: Option<Arc<WorktreeGuard>>,
    stream_sink: Option<Arc<dyn StreamSink>>,
    signals: Option<Arc<SignalBus>>,
    /// Override system prompt entirely (bypasses role-based template).
    override_prompt: Option<String>,
}

impl AgentBuilder {
    /// Create a new builder with the given agent name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            xaft_config: XaftAgentConfig::default(),
            extra_prompt: None,
            tools: Vec::new(),
            git_guard: None,
            stream_sink: None,
            signals: None,
            override_prompt: None,
        }
    }

    /// Set the agent role (determines default system prompt).
    pub fn role(mut self, role: AgentRole) -> Self {
        self.xaft_config.role = role;
        self
    }

    /// Append extra instructions to the role's default system prompt.
    pub fn system_prompt_extra(mut self, extra: impl Into<String>) -> Self {
        self.extra_prompt = Some(extra.into());
        self
    }

    /// Fully override the system prompt (bypasses role-based template).
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.override_prompt = Some(prompt.into());
        self
    }

    /// Set the tool list.
    pub fn tools(mut self, tools: Vec<Arc<ErasedTool>>) -> Self {
        self.tools = tools;
        self
    }

    /// Append a single tool.
    pub fn tool(mut self, tool: Arc<ErasedTool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Attach a `WorktreeGuard` for git auto-commit.
    pub fn with_git_guard(mut self, guard: Arc<WorktreeGuard>) -> Self {
        self.git_guard = Some(guard);
        self
    }

    /// Commit on successful completion (`StopReason::EndTurn`).
    pub fn commit_on_success(mut self) -> Self {
        self.xaft_config.commit_policy = CommitPolicy::OnSuccess;
        self
    }

    /// Always commit regardless of stop reason.
    pub fn commit_always(mut self) -> Self {
        self.xaft_config.commit_policy = CommitPolicy::Always;
        self
    }

    /// Set a custom commit policy.
    pub fn commit_policy(mut self, policy: CommitPolicy) -> Self {
        self.xaft_config.commit_policy = policy;
        self
    }

    /// Attach a stream sink for forwarding events.
    pub fn stream_sink<S: StreamSink>(mut self, sink: S) -> Self {
        self.stream_sink = Some(Arc::new(sink) as Arc<dyn StreamSink>);
        self
    }

    /// Attach a shared stream sink (already Arc-wrapped).
    pub fn stream_sink_arc(mut self, sink: Arc<dyn StreamSink>) -> Self {
        self.stream_sink = Some(sink);
        self
    }

    /// Attach a shared signal bus.
    pub fn signals(mut self, bus: Arc<SignalBus>) -> Self {
        self.signals = Some(bus);
        self
    }

    /// Maximum ReAct turns.
    pub fn max_turns(mut self, n: usize) -> Self {
        self.xaft_config.max_turns = n;
        self
    }

    /// Enable parallel tool calls using the `All` policy.
    /// Prefer `with_parallel_policy` for fine-grained control.
    pub fn parallel_tools(mut self) -> Self {
        self.xaft_config.parallel_tool_policy = agtrs_runtime::agent::ParallelToolPolicy::All;
        self
    }

    /// Set the parallel tool policy.
    pub fn with_parallel_policy(
        mut self,
        policy: agtrs_runtime::agent::ParallelToolPolicy,
    ) -> Self {
        self.xaft_config.parallel_tool_policy = policy;
        self
    }

    /// Set the maximum number of concurrent tool executions.
    pub fn max_concurrent_tools(mut self, n: usize) -> Self {
        self.xaft_config.max_concurrent_tools = n;
        self
    }

    /// Set a hard deadline.
    pub fn deadline(mut self, d: Duration) -> Self {
        self.xaft_config.deadline = Some(d);
        self
    }

    /// Cap cost per run.
    pub fn cost_limit(mut self, usd: f64) -> Self {
        self.xaft_config.cost_limit_usd = Some(usd);
        self
    }

    /// Override max tokens per LLM response.
    pub fn max_tokens_per_turn(mut self, n: u32) -> Self {
        self.xaft_config.max_tokens_per_turn = n;
        self
    }

    /// Set LLM temperature.
    pub fn temperature(mut self, t: f32) -> Self {
        self.xaft_config.temperature = t;
        self
    }

    /// Consume the builder and produce a [`XaftAgent`].
    pub fn build(self) -> XaftAgent {
        if let Some(prompt) = self.override_prompt {
            // Fully custom prompt — bypass the role-based template
            let mut agent = XaftAgent::from_xaft_config(
                self.name,
                self.xaft_config,
                None,
                self.tools,
                self.git_guard,
                self.stream_sink,
                self.signals,
            );
            agent.config.system_prompt = prompt;
            agent
        } else {
            XaftAgent::from_xaft_config(
                self.name,
                self.xaft_config,
                self.extra_prompt.as_deref(),
                self.tools,
                self.git_guard,
                self.stream_sink,
                self.signals,
            )
        }
    }
}

// ── PlanAgentBuilder ──────────────────────────────────────────────────────────

/// Fluent builder for [`PlanModeAgent`].
pub struct PlanAgentBuilder {
    agent_builder: AgentBuilder,
    plan_config: PlanModeConfig,
    resolve_ctx: Option<Arc<injectable_runtime::ResolveContext>>,
}

impl PlanAgentBuilder {
    /// Create a new plan-agent builder.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            agent_builder: AgentBuilder::new(name),
            plan_config: PlanModeConfig::default(),
            resolve_ctx: None,
        }
    }

    /// Set agent role.
    pub fn role(mut self, role: AgentRole) -> Self {
        self.agent_builder = self.agent_builder.role(role);
        self
    }

    /// Append extra prompt instructions.
    pub fn system_prompt_extra(mut self, extra: impl Into<String>) -> Self {
        self.agent_builder = self.agent_builder.system_prompt_extra(extra);
        self
    }

    /// Fully override the system prompt.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.agent_builder = self.agent_builder.system_prompt(prompt);
        self
    }

    /// Set the tool list.
    pub fn tools(mut self, tools: Vec<Arc<ErasedTool>>) -> Self {
        self.agent_builder = self.agent_builder.tools(tools);
        self
    }

    /// Append a single tool.
    pub fn tool(mut self, tool: Arc<ErasedTool>) -> Self {
        self.agent_builder = self.agent_builder.tool(tool);
        self
    }

    /// Attach a `WorktreeGuard` for git auto-commit.
    pub fn with_git_guard(mut self, guard: Arc<WorktreeGuard>) -> Self {
        self.agent_builder = self.agent_builder.with_git_guard(guard);
        self
    }

    /// Commit on success.
    pub fn commit_on_success(mut self) -> Self {
        self.agent_builder = self.agent_builder.commit_on_success();
        self
    }

    /// Attach a stream sink.
    pub fn stream_sink<S: StreamSink>(mut self, sink: S) -> Self {
        self.agent_builder = self.agent_builder.stream_sink(sink);
        self
    }

    /// Attach a shared signal bus.
    pub fn signals(mut self, bus: Arc<SignalBus>) -> Self {
        self.agent_builder = self.agent_builder.signals(bus);
        self
    }

    /// Maximum ReAct turns.
    pub fn max_turns(mut self, n: usize) -> Self {
        self.agent_builder = self.agent_builder.max_turns(n);
        self
    }

    /// Control when to escalate from OneShotPlanner to IterativeRefinementPlanner.
    pub fn escalation_policy(mut self, policy: EscalationPolicy) -> Self {
        self.plan_config.escalation_policy = policy;
        self
    }

    /// Set maximum iterative refinement cycles.
    pub fn max_refinement_iterations(mut self, n: usize) -> Self {
        self.plan_config.max_refinement_iterations = n;
        self
    }

    /// Disable plan message injection (agent won't see structured plan context).
    pub fn no_plan_injection(mut self) -> Self {
        self.plan_config.inject_plan_message = false;
        self
    }

    /// Attach a DI resolve context for `OneShotPlanner`'s tool-call strategy.
    pub fn resolve_ctx(mut self, ctx: Arc<injectable_runtime::ResolveContext>) -> Self {
        self.resolve_ctx = Some(ctx);
        self
    }

    /// Consume the builder and produce a [`PlanModeAgent`].
    pub fn build(self) -> PlanModeAgent {
        let resolve_ctx = self.resolve_ctx.unwrap_or_else(|| {
            Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
                injectable_runtime::EmptySingletonStore,
            )))
        });
        PlanModeAgent {
            inner: self.agent_builder.build(),
            plan_config: self.plan_config,
            resolve_ctx,
        }
    }
}

// ── Convenience constructors ──────────────────────────────────────────────────

impl XaftAgent {
    /// Create a minimal `XaftAgent` directly from an `AgentConfig`.
    ///
    /// Useful in tests where full builder configuration is unnecessary.
    pub fn from_config_direct(
        name: impl Into<String>,
        config: AgentConfig,
        tools: Vec<Arc<ErasedTool>>,
    ) -> Self {
        Self::new_with_config(name, config, tools)
    }
}
