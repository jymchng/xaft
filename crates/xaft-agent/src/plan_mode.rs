//! `PlanModeAgent` — a `XaftAgent` that plans before executing.
//!
//! # Planning cascade
//!
//! 1. **`OneShotPlanner`** — single LLM call, fast, works well for scoped tasks.
//! 2. **`IterativeRefinementPlanner`** — 1 + 2·N calls; triggered when
//!    `EscalationPolicy` says the one-shot plan is insufficient.
//!
//! The chosen plan is injected into `AgentContext` as a structured context
//! message before the first LLM execution turn.
//!
//! # Usage
//!
//! ```rust,ignore
//! use xaft_agent::builder::PlanAgentBuilder;
//! use xaft_agent::config::AgentRole;
//!
//! let agent = PlanAgentBuilder::new("plan-coder")
//!     .role(AgentRole::Coder)
//!     .tools(tools)
//!     .with_git_guard(guard)
//!     .max_refinement_iterations(3)
//!     .build();
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{instrument, warn};

use agtrs_runtime::agent::{Agent, AgentConfig, AgentContext, AgentResponse};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::executor::AgentExecutor;
use agtrs_runtime::llm::LlmOptions;
use agtrs_runtime::planner::{
    IterativeRefinementPlanner, OneShotPlanner, Planner, PlannerContext,
};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::task::{Intent, Plan};
use agtrs_runtime::tool::{ErasedTool, ToolResult};
use agtrs_runtime::transport::{Message, TokenUsage};

use crate::agent::XaftAgent;
use crate::config::{EscalationPolicy, PlanModeConfig};
use crate::signals::{XaftPlanCreated, XaftPlanEmpty};

// ── PlanModeAgent ─────────────────────────────────────────────────────────────

/// A `XaftAgent` that plans before executing.
///
/// Overrides `run()` to extract the user's goal, produce a plan via the
/// planner cascade, and inject the plan as context before handing off to
/// `AgentExecutor`.
pub struct PlanModeAgent {
    pub(crate) inner: XaftAgent,
    pub(crate) plan_config: PlanModeConfig,
    /// DI resolve context for `OneShotPlanner`'s tool-call strategy.
    pub(crate) resolve_ctx: Arc<injectable_runtime::ResolveContext>,
}

impl std::fmt::Debug for PlanModeAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanModeAgent")
            .field("inner", &self.inner)
            .field("plan_config", &self.plan_config)
            .finish()
    }
}

impl PlanModeAgent {
    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Extract the user's goal text from the input message.
    fn extract_goal(input: &Message) -> String {
        input.text()
    }

    /// Decide whether to escalate based on the plan and policy.
    fn should_escalate(plan: &Plan, policy: &EscalationPolicy) -> bool {
        match policy {
            EscalationPolicy::Never => false,
            EscalationPolicy::Always => true,
            EscalationPolicy::OnEmptyPlan => plan.steps.is_empty(),
            EscalationPolicy::OnFewerThan(n) => plan.steps.len() < *n,
        }
    }

    /// Format a plan as a human-readable context message to inject before the
    /// user's input so the executing agent understands what to do.
    fn format_plan_message(plan: &Plan) -> String {
        let mut out = String::from(
            "[Execution Plan]\n\
             The following ordered steps have been planned to accomplish your goal. \
             Execute them in sequence, using the specified tools.\n\n",
        );
        for (i, step) in plan.steps.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} (tool: `{}`",
                i + 1,
                step.description,
                step.tool_name
            ));
            if !step.input.is_null() && step.input != serde_json::json!({}) {
                out.push_str(&format!("  input: {}", step.input));
            }
            out.push_str(")\n");
        }
        out.push_str(
            "\nWhen executing, follow this plan precisely. \
             If a step fails, explain the failure and adapt as needed.",
        );
        out
    }

    /// Try the signal bus from either the inner agent or the context.
    fn signals(&self) -> Option<&Arc<SignalBus>> {
        self.inner.signals.as_ref()
    }

    fn try_emit_signal<S: Clone + Send + Sync + 'static>(&self, signal: S) {
        if let Some(bus) = self.signals() {
            let bus = Arc::clone(bus);
            tokio::spawn(async move {
                bus.emit(signal).await;
            });
        }
    }

    /// Run the full planning cascade using the LLM from `ctx`.
    ///
    /// Returns `(plan, strategy_name)` where strategy name is `"one_shot"` or
    /// `"iterative"`. Never panics — always returns a (possibly empty) plan.
    #[instrument(name = "plan_mode_plan", skip_all, fields(agent = %self.inner.name()))]
    async fn build_plan(
        &self,
        goal: &str,
        ctx: &AgentContext,
    ) -> (Plan, &'static str, String) {
        let llm = ctx.llm().clone();
        let tool_names: Vec<String> = self.inner.tools.iter().map(|t| t.name().into()).collect();

        let intent = Intent::from_goal(goal).build();
        let planner_ctx = PlannerContext::initial(&intent, tool_names);

        // ── Phase 1: OneShotPlanner ───────────────────────────────────────────
        // Skip one-shot if policy says always use iterative
        if !matches!(self.plan_config.escalation_policy, EscalationPolicy::Always) {
            let one_shot = OneShotPlanner::new(Arc::clone(&llm))
                .with_resolve_ctx(Arc::clone(&self.resolve_ctx));

            match one_shot.plan(&planner_ctx).await {
                Ok(plan) => {
                    if !Self::should_escalate(&plan, &self.plan_config.escalation_policy) {
                        return (plan, "one_shot", String::new());
                    }
                    tracing::debug!(
                        agent = %self.inner.name(),
                        steps = plan.steps.len(),
                        "xaft: one-shot plan insufficient, escalating to iterative refinement"
                    );
                }
                Err(e) => {
                    if matches!(self.plan_config.escalation_policy, EscalationPolicy::Never) {
                        // Never escalate — return empty plan rather than falling through
                        warn!(agent = %self.inner.name(), error = %e, "xaft: OneShotPlanner failed (Never escalation), returning empty plan");
                        return (Plan::empty(), "one_shot", format!("planning failed: {e}"));
                    }
                    warn!(agent = %self.inner.name(), error = %e, "xaft: OneShotPlanner failed, escalating to iterative");
                }
            }

            // If policy is Never, we already returned above; reaching here means
            // escalation is OnEmptyPlan/OnFewerThan/Always and we need to escalate.
            if matches!(self.plan_config.escalation_policy, EscalationPolicy::Never) {
                return (Plan::empty(), "one_shot", "escalation disabled".into());
            }
        }

        // ── Phase 2: IterativeRefinementPlanner ───────────────────────────────
        let iterative = IterativeRefinementPlanner::new(Arc::clone(&llm))
            .with_max_iterations(self.plan_config.max_refinement_iterations);

        match iterative.plan(&planner_ctx).await {
            Ok(plan) => {
                let rationale = String::new();
                (plan, "iterative", rationale)
            }
            Err(e) => {
                warn!(
                    agent = %self.inner.name(),
                    error = %e,
                    "xaft: IterativeRefinementPlanner failed, running without plan"
                );
                (Plan::empty(), "iterative", format!("planning failed: {e}"))
            }
        }
    }
}

// ── Agent impl ────────────────────────────────────────────────────────────────

#[async_trait]
impl Agent for PlanModeAgent {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn system_prompt(&self) -> String {
        self.inner.system_prompt()
    }

    fn tools(&self) -> Vec<Arc<ErasedTool>> {
        self.inner.tools()
    }

    fn config(&self) -> &AgentConfig {
        self.inner.config()
    }

    /// Plan first, then execute.
    ///
    /// 1. Extract goal from `input`
    /// 2. Run planning cascade (OneShotPlanner → IterativeRefinementPlanner)
    /// 3. Optionally inject plan as context message
    /// 4. Delegate to `AgentExecutor::run()`
    async fn run(
        &self,
        input: Message,
        ctx: &mut AgentContext,
    ) -> Result<AgentResponse, AgtrsError> {
        let goal = Self::extract_goal(&input);
        tracing::info!(agent = %self.name(), goal = %goal, "xaft: PlanModeAgent starting");

        // Plan
        let (plan, strategy, rationale) = self.build_plan(&goal, ctx).await;

        if plan.steps.is_empty() {
            tracing::warn!(agent = %self.name(), "xaft: plan is empty, running without plan context");
            self.try_emit_signal(XaftPlanEmpty {
                agent_name: self.name().to_string(),
                reason: if rationale.is_empty() {
                    "plan produced 0 steps".to_string()
                } else {
                    rationale.clone()
                },
            });
        } else {
            tracing::info!(
                agent = %self.name(),
                steps = plan.steps.len(),
                strategy,
                "xaft: plan produced"
            );
            self.try_emit_signal(XaftPlanCreated {
                agent_name: self.name().to_string(),
                steps: plan.steps.len(),
                strategy: strategy.to_string(),
                rationale: rationale.clone(),
            });

            if self.plan_config.inject_plan_message {
                let plan_msg = Self::format_plan_message(&plan);
                // Inject plan context before the user's input so the LLM sees it first
                ctx.add_message(Message::user(plan_msg));
            }
        }

        // Run
        AgentExecutor::run(self, input, ctx).await
    }

    // Delegate lifecycle hooks to inner XaftAgent

    async fn on_start(&self, ctx: &mut AgentContext) -> Result<(), AgtrsError> {
        self.inner.on_start(ctx).await
    }

    async fn before_llm_call(
        &self,
        messages: &mut Vec<Message>,
        options: &mut LlmOptions,
    ) -> Result<(), AgtrsError> {
        self.inner.before_llm_call(messages, options).await
    }

    async fn on_tool_result(
        &self,
        result: &ToolResult,
        ctx: &AgentContext,
    ) -> Result<(), AgtrsError> {
        self.inner.on_tool_result(result, ctx).await
    }

    async fn on_turn_complete(
        &self,
        usage: &TokenUsage,
        ctx: &AgentContext,
    ) -> Result<(), AgtrsError> {
        self.inner.on_turn_complete(usage, ctx).await
    }

    async fn on_finish(
        &self,
        response: &AgentResponse,
        ctx: &AgentContext,
    ) -> Result<(), AgtrsError> {
        self.inner.on_finish(response, ctx).await
    }
}
