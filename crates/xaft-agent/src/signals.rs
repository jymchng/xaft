//! xaft-specific signals emitted by `XaftAgent` and `PlanModeAgent`.
//!
//! These complement the agtrs signals (e.g. `ModelCallStarted`) with
//! xaft-level events about git commits, plans, and turn lifecycle.
//!
//! Subscribe via the shared `SignalBus`:
//!
//! ```rust,ignore
//! bus.on::<XaftCommitCreated>(|e| println!("committed: {}", e.sha)).await;
//! ```

use serde::{Deserialize, Serialize};

/// Emitted in `before_llm_call` when the agent is about to make an LLM call.
///
/// Useful for showing "thinking…" indicators in UIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftLlmCallStarting {
    /// The agent's name.
    pub agent_name: String,
    /// Zero-based LLM call number within this run.
    pub call_index: usize,
}

/// Emitted in `on_finish` when git auto-commit succeeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftCommitCreated {
    /// The agent's name.
    pub agent_name: String,
    /// Short commit SHA (first 8 chars).
    pub short_sha: String,
    /// The auto-generated or supplied commit message.
    pub message: String,
    /// Number of files changed.
    pub files_changed: usize,
    /// Lines added.
    pub lines_added: usize,
    /// Lines removed.
    pub lines_removed: usize,
}

/// Emitted in `PlanModeAgent::run()` when a plan is produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftPlanCreated {
    /// The agent's name.
    pub agent_name: String,
    /// Number of steps in the plan.
    pub steps: usize,
    /// Strategy used: `"one_shot"` or `"iterative"`.
    pub strategy: String,
    /// Brief rationale from the planner.
    pub rationale: String,
}

/// Emitted when planning produces an empty or failed plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftPlanEmpty {
    /// The agent's name.
    pub agent_name: String,
    /// Why the plan was empty.
    pub reason: String,
}

/// Emitted in `on_finish` with the agent's final text response.
///
/// The TUI subscribes to this to display agent output without streaming.
/// Empty content is NOT emitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftAgentOutput {
    /// The agent's name (e.g. `"coder"`, `"qa"`, `"fixer"`).
    pub agent_name: String,
    /// The agent's final text response.
    pub content: String,
}
