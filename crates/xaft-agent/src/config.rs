//! Configuration types for xaft agents.

use std::time::Duration;

/// The role an agent plays in the system.
///
/// Each role maps to a curated default system prompt and tool set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRole {
    /// Writes, edits, and reasons about source code.
    Coder,
    /// Reviews diffs and proposes improvements without modifying files.
    Reviewer,
    /// Decomposes high-level goals into ordered step lists.
    Planner,
    /// Coordinates multiple sub-agents and delegates work.
    Orchestrator,
    /// User-supplied role with a custom name (no default prompt).
    Custom(String),
}

impl AgentRole {
    /// Short identifier used in logging and agent naming.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Coder => "coder",
            Self::Reviewer => "reviewer",
            Self::Planner => "planner",
            Self::Orchestrator => "orchestrator",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Policy for when the agent auto-commits via its `WorktreeGuard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitPolicy {
    /// Never auto-commit (default for review/planner roles).
    Never,
    /// Commit when the agent finishes successfully (`StopReason::EndTurn`).
    OnSuccess,
    /// Always commit regardless of stop reason.
    Always,
}

impl Default for CommitPolicy {
    fn default() -> Self {
        Self::Never
    }
}

/// Policy for when planning should escalate from `OneShotPlanner` to
/// `IterativeRefinementPlanner`.
#[derive(Debug, Clone)]
pub enum EscalationPolicy {
    /// Escalate if `OneShotPlanner` returns 0 steps.
    OnEmptyPlan,
    /// Escalate if `OneShotPlanner` returns fewer than `n` steps.
    OnFewerThan(usize),
    /// Never escalate — always use `OneShotPlanner` only.
    Never,
    /// Always use `IterativeRefinementPlanner` (skip `OneShotPlanner`).
    Always,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self::OnEmptyPlan
    }
}

/// Configuration specific to plan-mode agents.
#[derive(Debug, Clone)]
pub struct PlanModeConfig {
    /// Strategy for escalating from OneShotPlanner to IterativeRefinementPlanner.
    pub escalation_policy: EscalationPolicy,
    /// Maximum critique-revise cycles when using `IterativeRefinementPlanner`.
    pub max_refinement_iterations: usize,
    /// Inject the plan as a structured context message before the user's input.
    pub inject_plan_message: bool,
    /// Register the `ReplanTool` so the agent can trigger mid-execution re-planning.
    pub enable_replan_tool: bool,
}

impl Default for PlanModeConfig {
    fn default() -> Self {
        Self {
            escalation_policy: EscalationPolicy::OnEmptyPlan,
            max_refinement_iterations: 2,
            inject_plan_message: true,
            enable_replan_tool: false, // replan tool needs task runner, opt-in
        }
    }
}

/// High-level xaft agent configuration that maps onto `agtrs::AgentConfig`.
#[derive(Debug, Clone)]
pub struct XaftAgentConfig {
    /// Agent role — determines default system prompt when none is set.
    pub role: AgentRole,

    /// When to auto-commit via `WorktreeGuard`.
    pub commit_policy: CommitPolicy,

    /// Maximum ReAct loop iterations.
    pub max_turns: usize,

    /// Whether to allow the executor to run tools in parallel.
    pub parallel_tools: bool,

    /// Hard wall-clock limit for the entire agent run.
    pub deadline: Option<Duration>,

    /// Maximum spend in USD per run.
    pub cost_limit_usd: Option<f64>,

    /// Maximum tokens per LLM response.
    pub max_tokens_per_turn: u32,

    /// LLM sampling temperature.
    pub temperature: f32,

    /// Auto-approve tool calls for the named tools (skip approval gates).
    pub auto_approve_tools: Vec<String>,
}

impl Default for XaftAgentConfig {
    fn default() -> Self {
        Self {
            role: AgentRole::Coder,
            commit_policy: CommitPolicy::Never,
            max_turns: 20,
            parallel_tools: false,
            deadline: None,
            cost_limit_usd: None,
            max_tokens_per_turn: 8192,
            temperature: 1.0,
            auto_approve_tools: Vec::new(),
        }
    }
}

impl XaftAgentConfig {
    /// Create a config for the given role with defaults.
    pub fn for_role(role: AgentRole) -> Self {
        let commit_policy = match role {
            AgentRole::Coder => CommitPolicy::OnSuccess,
            _ => CommitPolicy::Never,
        };
        Self {
            role,
            commit_policy,
            ..Default::default()
        }
    }
}
