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
    resolve_ctx: Option<Arc<injectable_rs_runtime::ResolveContext>>,
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
    pub fn resolve_ctx(mut self, ctx: Arc<injectable_rs_runtime::ResolveContext>) -> Self {
        self.resolve_ctx = Some(ctx);
        self
    }

    /// Consume the builder and produce a [`PlanModeAgent`].
    pub fn build(self) -> PlanModeAgent {
        let resolve_ctx = self.resolve_ctx.unwrap_or_else(|| {
            Arc::new(injectable_rs_runtime::ResolveContext::from_store(Arc::new(
                injectable_rs_runtime::EmptySingletonStore,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{CODER_SYSTEM_PROMPT, REVIEWER_SYSTEM_PROMPT};
    use crate::stream::NopSink;
    use agtrs_runtime::agent::ParallelToolPolicy;
    use async_trait::async_trait;

    // ── defaults ────────────────────────────────────────────────────────

    #[test]
    fn a_fresh_builder_produces_the_documented_defaults() {
        let agent = AgentBuilder::new("a").build();
        assert_eq!(agent.config.max_turns, 20);
        assert_eq!(agent.config.max_tokens_per_turn, 8192);
        assert_eq!(agent.config.temperature, 1.0);
        assert_eq!(agent.config.max_concurrent_tools, 4);
        assert_eq!(
            agent.config.parallel_tool_policy,
            ParallelToolPolicy::Annotated
        );
        assert!(agent.config.deadline.is_none());
        assert!(agent.config.max_cost_usd.is_none());
    }

    #[test]
    fn a_fresh_builder_never_auto_commits_and_wires_no_optional_dependency() {
        // CommitPolicy::Never is the default, so auto-commit is opt-in. The
        // three Option fields are None because each is opt-in too: a builder
        // that silently attached a guard or a sink would make
        // `git_guard.is_none()` meaningless as a signal.
        let agent = AgentBuilder::new("a").build();
        assert_eq!(agent.commit_policy(), &CommitPolicy::Never);
        assert!(agent.git_guard.is_none());
        assert!(agent.stream_sink.is_none());
        assert!(agent.signals.is_none());
    }

    #[test]
    fn a_fresh_builder_has_no_tools() {
        assert!(AgentBuilder::new("a").build().tools.is_empty());
    }

    // ── role → system prompt ─────────────────────────────────────────────

    #[test]
    fn role_selects_the_default_system_prompt() {
        // This is the whole point of `role`: the built agent's prompt comes
        // from the role template, not from a fixed default.
        let coder = AgentBuilder::new("a").role(AgentRole::Coder).build();
        let reviewer = AgentBuilder::new("a").role(AgentRole::Reviewer).build();
        assert_eq!(coder.config.system_prompt, CODER_SYSTEM_PROMPT);
        assert_eq!(reviewer.config.system_prompt, REVIEWER_SYSTEM_PROMPT);
    }

    #[test]
    fn a_custom_role_has_no_template_and_yields_an_empty_prompt() {
        // `Custom` deliberately maps to no template (default_prompt_for
        // returns None), so the prompt is empty unless the caller supplies
        // one. Documented behaviour, not an accident.
        let agent = AgentBuilder::new("a")
            .role(AgentRole::Custom("arch-reviewer".into()))
            .build();
        assert!(agent.config.system_prompt.is_empty());
    }

    #[test]
    fn system_prompt_extra_is_appended_after_the_role_template() {
        let agent = AgentBuilder::new("a")
            .role(AgentRole::Coder)
            .system_prompt_extra("Always use Rust 2024 edition.")
            .build();
        let p = &agent.config.system_prompt;
        assert!(
            p.starts_with(CODER_SYSTEM_PROMPT),
            "template must come first"
        );
        assert!(p.contains("## Additional Instructions"));
        assert!(p.contains("Rust 2024 edition."));
    }

    #[test]
    fn a_blank_extra_does_not_append_a_section() {
        // build_system_prompt treats an empty/whitespace extra as absent. If
        // this regressed, every agent built with `.system_prompt_extra("")`
        // would carry an empty "## Additional Instructions" heading.
        let agent = AgentBuilder::new("a")
            .role(AgentRole::Coder)
            .system_prompt_extra("   \n  ")
            .build();
        assert_eq!(agent.config.system_prompt, CODER_SYSTEM_PROMPT);
    }

    // ── system_prompt override ───────────────────────────────────────────

    #[test]
    fn system_prompt_fully_replaces_the_role_template() {
        // The override must bypass the template entirely. If it merely
        // prepended or appended, the agent would still be steered by the
        // Coder role — a silent, hard-to-debug misconfiguration.
        let agent = AgentBuilder::new("a")
            .role(AgentRole::Coder)
            .system_prompt("You are a release-notes bot.")
            .build();
        assert_eq!(agent.config.system_prompt, "You are a release-notes bot.");
        assert!(!agent.config.system_prompt.contains("engineer"));
    }

    #[test]
    fn the_override_wins_over_extra_because_extra_only_feeds_the_template() {
        // Both setters can be called. `extra_prompt` is passed into
        // build_system_prompt, which only runs on the non-override path, so
        // the override branch must win rather than combining both. This pins
        // which of the two a caller who set both actually gets.
        let agent = AgentBuilder::new("a")
            .role(AgentRole::Coder)
            .system_prompt_extra("ignored extra")
            .system_prompt("only this")
            .build();
        assert_eq!(agent.config.system_prompt, "only this");
    }

    #[test]
    fn the_override_wins_regardless_of_call_order() {
        // Builders are order-independent by convention; prove it for the one
        // pair of setters that share a destination field.
        let a = AgentBuilder::new("a")
            .system_prompt("only this")
            .system_prompt_extra("ignored extra")
            .build();
        assert_eq!(a.config.system_prompt, "only this");
    }

    // ── tools ────────────────────────────────────────────────────────────

    #[test]
    fn a_single_tool_is_appended_to_the_list() {
        let agent = AgentBuilder::new("a").tool(noop()).build();
        assert_eq!(agent.tools.len(), 1);
        assert_eq!(agent.tools[0].name(), "noop");
    }

    #[test]
    fn repeated_tool_calls_accumulate_in_order() {
        let agent = AgentBuilder::new("a")
            .tool(NoopTool::named("first"))
            .tool(NoopTool::named("second"))
            .build();
        assert_eq!(agent.tools.len(), 2);
        assert_eq!(agent.tools[0].name(), "first");
        assert_eq!(agent.tools[1].name(), "second");
    }

    #[test]
    fn tools_replaces_anything_appended_before_it() {
        // `tools` replaces the whole list, so a later `tools` call discards
        // anything an earlier `tool` added. Order-sensitive by design; this
        // is the test that catches someone "fixing" it into appending.
        let agent = AgentBuilder::new("a").tool(noop()).tools(vec![]).build();
        assert!(agent.tools.is_empty());
    }

    #[test]
    fn tools_installs_the_supplied_list_verbatim() {
        let agent = AgentBuilder::new("a").tools(vec![noop(), noop()]).build();
        assert_eq!(agent.tools.len(), 2);
    }

    // ── commit policy ────────────────────────────────────────────────────

    #[test]
    fn commit_on_success_sets_on_success() {
        let agent = AgentBuilder::new("a").commit_on_success().build();
        assert_eq!(agent.commit_policy(), &CommitPolicy::OnSuccess);
    }

    #[test]
    fn commit_always_sets_always() {
        let agent = AgentBuilder::new("a").commit_always().build();
        assert_eq!(agent.commit_policy(), &CommitPolicy::Always);
    }

    #[test]
    fn commit_policy_sets_an_arbitrary_policy_and_the_last_call_wins() {
        let agent = AgentBuilder::new("a")
            .commit_policy(CommitPolicy::OnSuccess)
            .commit_policy(CommitPolicy::Never)
            .build();
        assert_eq!(agent.commit_policy(), &CommitPolicy::Never);
    }

    // ── numeric / sampling knobs reach AgentConfig ───────────────────────

    #[test]
    fn max_turns_reaches_the_agent_config() {
        assert_eq!(
            AgentBuilder::new("a")
                .max_turns(42)
                .build()
                .config
                .max_turns,
            42
        );
    }

    #[test]
    fn max_tokens_per_turn_reaches_the_agent_config() {
        let agent = AgentBuilder::new("a").max_tokens_per_turn(512).build();
        assert_eq!(agent.config.max_tokens_per_turn, 512);
    }

    #[test]
    fn temperature_reaches_the_agent_config() {
        let agent = AgentBuilder::new("a").temperature(0.25).build();
        assert!((agent.config.temperature - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn max_concurrent_tools_reaches_the_agent_config() {
        let agent = AgentBuilder::new("a").max_concurrent_tools(16).build();
        assert_eq!(agent.config.max_concurrent_tools, 16);
    }

    #[test]
    fn deadline_reaches_the_agent_config() {
        let agent = AgentBuilder::new("a")
            .deadline(Duration::from_secs(90))
            .build();
        assert_eq!(agent.config.deadline, Some(Duration::from_secs(90)));
    }

    #[test]
    fn cost_limit_reaches_the_agent_config() {
        let agent = AgentBuilder::new("a").cost_limit(1.25).build();
        assert_eq!(agent.config.max_cost_usd, Some(1.25));
    }

    // ── parallel tool policy ─────────────────────────────────────────────

    #[test]
    fn parallel_tools_selects_the_all_policy() {
        let agent = AgentBuilder::new("a").parallel_tools().build();
        assert_eq!(agent.config.parallel_tool_policy, ParallelToolPolicy::All);
    }

    #[test]
    fn with_parallel_policy_selects_the_named_policy() {
        let agent = AgentBuilder::new("a")
            .with_parallel_policy(ParallelToolPolicy::Sequential)
            .build();
        assert_eq!(
            agent.config.parallel_tool_policy,
            ParallelToolPolicy::Sequential
        );
    }

    #[test]
    fn the_last_parallel_policy_call_wins() {
        let agent = AgentBuilder::new("a")
            .parallel_tools()
            .with_parallel_policy(ParallelToolPolicy::Sequential)
            .build();
        assert_eq!(
            agent.config.parallel_tool_policy,
            ParallelToolPolicy::Sequential
        );
    }

    #[test]
    fn parallel_tool_calls_stays_off_even_when_the_policy_allows_parallelism() {
        // from_xaft_config hard-sets `parallel_tool_calls: false` because the
        // policy is the only parallel gate. If that were left true, the
        // deprecated flag would silently re-enable concurrency for agents
        // that never asked for it.
        assert!(
            !AgentBuilder::new("a")
                .parallel_tools()
                .build()
                .config
                .parallel_tool_calls
        );
    }

    // ── optional attachments ─────────────────────────────────────────────

    #[test]
    fn stream_sink_arc_is_attached() {
        let sink: Arc<dyn StreamSink> = Arc::new(NopSink);
        let agent = AgentBuilder::new("a").stream_sink_arc(sink).build();
        assert!(agent.stream_sink.is_some());
    }

    #[test]
    fn a_by_value_stream_sink_is_arc_wrapped_and_attached() {
        let agent = AgentBuilder::new("a").stream_sink(NopSink).build();
        assert!(agent.stream_sink.is_some());
    }

    #[test]
    fn signals_are_attached() {
        let agent = AgentBuilder::new("a")
            .signals(Arc::new(SignalBus::new()))
            .build();
        assert!(agent.signals.is_some());
    }

    // ── PlanAgentBuilder ─────────────────────────────────────────────────

    #[test]
    fn a_fresh_plan_builder_uses_the_documented_plan_defaults() {
        let agent = PlanAgentBuilder::new("p").build();
        assert!(matches!(
            agent.plan_config.escalation_policy,
            EscalationPolicy::OnEmptyPlan
        ));
        assert_eq!(agent.plan_config.max_refinement_iterations, 2);
        assert!(agent.plan_config.inject_plan_message);
        assert!(!agent.plan_config.enable_replan_tool);
    }

    #[test]
    fn escalation_policy_is_carried_onto_the_plan_config() {
        let agent = PlanAgentBuilder::new("p")
            .escalation_policy(EscalationPolicy::OnFewerThan(3))
            .build();
        assert!(matches!(
            agent.plan_config.escalation_policy,
            EscalationPolicy::OnFewerThan(3)
        ));
    }

    #[test]
    fn escalation_policy_never_and_always_are_carried_unchanged() {
        let never = PlanAgentBuilder::new("p")
            .escalation_policy(EscalationPolicy::Never)
            .build();
        assert!(matches!(
            never.plan_config.escalation_policy,
            EscalationPolicy::Never
        ));

        let always = PlanAgentBuilder::new("p")
            .escalation_policy(EscalationPolicy::Always)
            .build();
        assert!(matches!(
            always.plan_config.escalation_policy,
            EscalationPolicy::Always
        ));
    }

    #[test]
    fn max_refinement_iterations_is_carried_onto_the_plan_config() {
        let agent = PlanAgentBuilder::new("p")
            .max_refinement_iterations(7)
            .build();
        assert_eq!(agent.plan_config.max_refinement_iterations, 7);
    }

    #[test]
    fn no_plan_injection_disables_the_plan_message() {
        let agent = PlanAgentBuilder::new("p").no_plan_injection().build();
        assert!(!agent.plan_config.inject_plan_message);
    }

    #[test]
    fn plan_builder_without_a_resolve_ctx_still_builds_one() {
        // The unwrap_or_else fallback must produce a usable context, or every
        // plan agent built without an explicit ctx panics later in
        // OneShotPlanner's tool-call strategy.
        let agent = PlanAgentBuilder::new("p").build();
        assert!(Arc::strong_count(&agent.resolve_ctx) >= 1);
    }

    #[test]
    fn a_supplied_resolve_ctx_is_the_one_kept() {
        // Not merely "some context exists" — the caller's exact Arc is
        // retained, because the planner resolves tools through it and a
        // silently substituted empty store would resolve nothing.
        let ctx = Arc::new(injectable_rs_runtime::ResolveContext::from_store(Arc::new(
            injectable_rs_runtime::EmptySingletonStore,
        )));
        let expected = Arc::clone(&ctx);
        let agent = PlanAgentBuilder::new("p").resolve_ctx(ctx).build();
        assert!(Arc::ptr_eq(&agent.resolve_ctx, &expected));
    }

    // ── PlanAgentBuilder delegates to the inner AgentBuilder ─────────────

    #[test]
    fn plan_builder_delegates_the_role_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p").role(AgentRole::Reviewer).build();
        assert_eq!(agent.inner.config.system_prompt, REVIEWER_SYSTEM_PROMPT);
    }

    #[test]
    fn plan_builder_delegates_max_turns_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p").max_turns(11).build();
        assert_eq!(agent.inner.config.max_turns, 11);
    }

    #[test]
    fn plan_builder_delegates_the_system_prompt_override_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p")
            .system_prompt("plan-mode only")
            .build();
        assert_eq!(agent.inner.config.system_prompt, "plan-mode only");
    }

    #[test]
    fn plan_builder_delegates_the_extra_prompt_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p")
            .role(AgentRole::Coder)
            .system_prompt_extra("be terse")
            .build();
        assert!(agent.inner.config.system_prompt.contains("be terse"));
    }

    #[test]
    fn plan_builder_delegates_commit_on_success_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p").commit_on_success().build();
        assert_eq!(agent.inner.commit_policy(), &CommitPolicy::OnSuccess);
    }

    #[test]
    fn plan_builder_delegates_signals_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p")
            .signals(Arc::new(SignalBus::new()))
            .build();
        assert!(agent.inner.signals.is_some());
    }

    #[test]
    fn plan_builder_delegates_a_single_tool_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p").tool(noop()).build();
        assert_eq!(agent.inner.tools.len(), 1);
    }

    #[test]
    fn plan_builder_delegates_the_tool_list_to_the_inner_agent() {
        let agent = PlanAgentBuilder::new("p")
            .tools(vec![noop(), noop()])
            .build();
        assert_eq!(agent.inner.tools.len(), 2);
    }

    // ── convenience constructor ──────────────────────────────────────────

    #[test]
    fn from_config_direct_uses_the_supplied_config_verbatim() {
        // This constructor exists so tests can build an agent without the
        // xaft-level config, so it must pass the config through untouched.
        let cfg = AgentConfig {
            system_prompt: "verbatim".into(),
            max_turns: 3,
            ..Default::default()
        };
        let agent = XaftAgent::from_config_direct("direct", cfg, vec![noop()]);
        assert_eq!(agent.config.system_prompt, "verbatim");
        assert_eq!(agent.config.max_turns, 3);
        assert_eq!(agent.tools.len(), 1);
        // new_with_config pins Never regardless of the supplied config, so
        // passing a config cannot accidentally turn auto-commit on.
        assert_eq!(agent.commit_policy(), &CommitPolicy::Never);
    }

    #[test]
    fn from_config_direct_defaults_every_optional_dependency_to_absent() {
        let agent = XaftAgent::from_config_direct("d", AgentConfig::default(), vec![]);
        assert!(agent.git_guard.is_none());
        assert!(agent.stream_sink.is_none());
        assert!(agent.signals.is_none());
    }

    // ── helpers ──────────────────────────────────────────────────────────

    /// A named no-op `ErasedTool`, used only to assert list wiring. `name` is
    /// a field so callers can distinguish two instances in one list.
    struct NoopTool {
        name: &'static str,
    }

    impl NoopTool {
        fn named(name: &'static str) -> Arc<ErasedTool> {
            Arc::new(NoopTool { name }) as Arc<ErasedTool>
        }
    }

    fn noop() -> Arc<ErasedTool> {
        NoopTool::named("noop")
    }

    #[async_trait]
    impl agtrs_runtime::tool::Tool for NoopTool {
        type Inputs = serde_json::Value;
        type Output = agtrs_runtime::tool::ToolResult;

        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "does nothing"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(
            &self,
            _input: serde_json::Value,
            ctx: &agtrs_runtime::tool::ToolContext,
        ) -> Result<agtrs_runtime::tool::ToolResult, agtrs_runtime::error::AgtrsError> {
            Ok(agtrs_runtime::tool::ToolResult::ok(
                "noop",
                &ctx.tool_use_id,
            ))
        }
    }
}
