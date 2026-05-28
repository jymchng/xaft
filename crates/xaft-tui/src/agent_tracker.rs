//! Agent activity tracker — maintains per-agent state updated from `TuiEvent`s.
//!
//! `AgentTracker` is the single source of truth for the `AgentActivityWidget`.
//! It is updated by `AppState::handle_event` and read by the renderer.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

// ── AgentStatus ───────────────────────────────────────────────────────────────

/// Agent status in the activity tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Not yet active or finished waiting.
    Idle,
    /// LLM inference in progress.
    Thinking,
    /// Tool execution in progress.
    ToolCalling,
    /// Blocked waiting for user approval.
    AwaitingApproval,
    /// Run completed successfully.
    Done,
    /// Run ended with an error.
    Failed,
    /// Run was cancelled.
    Cancelled,
}

impl AgentStatus {
    /// Single-character icon for this status.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Idle => "○",
            Self::Thinking => "⏳",
            Self::ToolCalling => "⚙",
            Self::AwaitingApproval => "⚠",
            Self::Done => "✓",
            Self::Failed => "✗",
            Self::Cancelled => "⊘",
        }
    }

    /// Crossterm color for this status.
    pub fn color(self) -> crossterm::style::Color {
        use crossterm::style::Color;
        match self {
            Self::Idle => Color::Rgb {
                r: 120,
                g: 120,
                b: 120,
            },
            Self::Thinking => Color::Rgb {
                r: 220,
                g: 180,
                b: 80,
            },
            Self::ToolCalling => Color::Rgb {
                r: 86,
                g: 156,
                b: 214,
            },
            Self::AwaitingApproval => Color::Rgb {
                r: 220,
                g: 120,
                b: 60,
            },
            Self::Done => Color::Rgb {
                r: 78,
                g: 201,
                b: 176,
            },
            Self::Failed => Color::Rgb {
                r: 220,
                g: 80,
                b: 80,
            },
            Self::Cancelled => Color::Rgb {
                r: 150,
                g: 100,
                b: 150,
            },
        }
    }

    /// Whether this status counts as "actively working".
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Thinking | Self::ToolCalling | Self::AwaitingApproval
        )
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Thinking => "Thinking",
            Self::ToolCalling => "Tool",
            Self::AwaitingApproval => "Approval",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

// ── ToolCallInfo ──────────────────────────────────────────────────────────────

/// Info about a single tool call made by an agent.
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    /// Name of the tool (e.g. `"read_file"`).
    pub tool_name: String,
    /// Short truncated preview of the input.
    pub input_summary: String,
    /// Tool use id from the LLM response.
    pub tool_use_id: String,
    /// When the tool call started.
    pub started_at: Instant,
    /// When the tool call ended (`None` = still running).
    pub finished_at: Option<Instant>,
    /// `None` = still running; `Some(true)` = succeeded; `Some(false)` = failed.
    pub success: Option<bool>,
}

impl ToolCallInfo {
    /// Elapsed duration string: `"42ms"` or `"1.4s"`.
    pub fn elapsed_str(&self) -> String {
        let end = self.finished_at.unwrap_or_else(Instant::now);
        let ms = end.duration_since(self.started_at).as_millis();
        if ms >= 1000 {
            format!("{:.1}s", ms as f64 / 1000.0)
        } else {
            format!("{ms}ms")
        }
    }
}

// ── AgentNode ─────────────────────────────────────────────────────────────────

/// One agent's node in the activity tree.
#[derive(Debug, Clone)]
pub struct AgentNode {
    /// Agent name (e.g. `"coder"`, `"qa"`).
    pub name: String,
    /// Current status.
    pub status: AgentStatus,
    /// Tool calls in order (newest last) — capped at 20 entries.
    pub tool_history: VecDeque<ToolCallInfo>,
    /// Currently running tool (last history entry where `success == None`).
    pub current_tool: Option<ToolCallInfo>,
    /// Number of LLM turns started.
    pub turns: usize,
    /// Count of successfully completed tool calls.
    pub tool_calls_completed: usize,
    /// Count of failed tool calls.
    pub tool_calls_failed: usize,
    /// When this agent node was first created.
    pub started_at: Instant,
    /// When the current status was entered.
    pub status_changed_at: Instant,
    /// Cumulative cost in USD accumulated from `LlmCallComplete` events.
    pub cost_usd: f64,
}

impl AgentNode {
    /// Create a new node for `name` in `Idle` status.
    pub fn new(name: impl Into<String>) -> Self {
        let now = Instant::now();
        Self {
            name: name.into(),
            status: AgentStatus::Idle,
            tool_history: VecDeque::new(),
            current_tool: None,
            turns: 0,
            tool_calls_completed: 0,
            tool_calls_failed: 0,
            started_at: now,
            status_changed_at: now,
            cost_usd: 0.0,
        }
    }

    /// Duration the agent has been in the current status.
    pub fn status_duration(&self) -> Duration {
        self.status_changed_at.elapsed()
    }

    /// Total elapsed since this agent first appeared.
    pub fn total_duration(&self) -> Duration {
        self.started_at.elapsed()
    }
}

// ── AgentTracker ──────────────────────────────────────────────────────────────

/// Tracks all agents seen during a run, updated from `TuiEvent`s.
#[derive(Debug, Clone, Default)]
pub struct AgentTracker {
    /// Insertion-ordered list of agent names (display order).
    pub order: Vec<String>,
    /// Per-agent state indexed by name.
    pub nodes: HashMap<String, AgentNode>,
    /// When the first agent appeared (= run start).
    pub run_started_at: Option<Instant>,
}

impl AgentTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Find or create an agent node; returns a mutable reference.
    pub fn get_or_create(&mut self, name: &str) -> &mut AgentNode {
        if !self.nodes.contains_key(name) {
            if self.run_started_at.is_none() {
                self.run_started_at = Some(Instant::now());
            }
            self.order.push(name.to_string());
            self.nodes.insert(name.to_string(), AgentNode::new(name));
        }
        self.nodes.get_mut(name).expect("just inserted")
    }

    // ── Event handlers ────────────────────────────────────────────────────────

    /// Agent started a new LLM turn (`LlmCallStarting`).
    pub fn on_llm_start(&mut self, agent_name: &str) {
        let node = self.get_or_create(agent_name);
        node.status = AgentStatus::Thinking;
        node.status_changed_at = Instant::now();
        node.turns += 1;
    }

    /// Agent's LLM call completed (`LlmCallComplete`).
    ///
    /// Accumulates cost but does not change status — the agent may still issue
    /// tool calls before the turn ends.
    pub fn on_llm_complete(&mut self, agent_name: &str, cost_usd: f64) {
        if let Some(node) = self.nodes.get_mut(agent_name) {
            node.cost_usd += cost_usd;
        }
    }

    /// A tool call started, attributed to `agent_name` (`ToolStarted`).
    pub fn on_tool_start(
        &mut self,
        agent_name: &str,
        tool_name: &str,
        tool_use_id: &str,
        input_summary: &str,
    ) {
        let node = self.get_or_create(agent_name);
        let info = ToolCallInfo {
            tool_name: tool_name.into(),
            input_summary: input_summary.into(),
            tool_use_id: tool_use_id.into(),
            started_at: Instant::now(),
            finished_at: None,
            success: None,
        };
        node.current_tool = Some(info.clone());
        if node.tool_history.len() >= 20 {
            node.tool_history.pop_front();
        }
        node.tool_history.push_back(info);
        node.status = AgentStatus::ToolCalling;
        node.status_changed_at = Instant::now();
    }

    /// A tool call completed (`ToolCompleted`).
    pub fn on_tool_complete(&mut self, agent_name: &str, tool_use_id: &str, success: bool) {
        if let Some(node) = self.nodes.get_mut(agent_name) {
            if success {
                node.tool_calls_completed += 1;
            } else {
                node.tool_calls_failed += 1;
            }

            // Mark the current_tool if it matches
            if let Some(ref mut ct) = node.current_tool {
                if ct.tool_use_id == tool_use_id {
                    ct.finished_at = Some(Instant::now());
                    ct.success = Some(success);
                }
            }

            // Update the matching history entry
            if let Some(entry) = node
                .tool_history
                .iter_mut()
                .rev()
                .find(|t| t.tool_use_id == tool_use_id)
            {
                entry.finished_at = Some(Instant::now());
                entry.success = Some(success);
            }

            // Return to Thinking — agent will process the tool result
            if node.status == AgentStatus::ToolCalling {
                node.status = AgentStatus::Thinking;
                node.status_changed_at = Instant::now();
            }
        }
    }

    /// Agent is blocked awaiting user approval (`ToolPendingApproval`).
    pub fn on_approval_pending(&mut self, agent_name: &str) {
        if let Some(node) = self.nodes.get_mut(agent_name) {
            node.status = AgentStatus::AwaitingApproval;
            node.status_changed_at = Instant::now();
        }
    }

    /// Agent run completed successfully (`AgentRunComplete`).
    pub fn on_run_complete(&mut self, agent_name: &str) {
        if let Some(node) = self.nodes.get_mut(agent_name) {
            node.status = AgentStatus::Done;
            node.status_changed_at = Instant::now();
            node.current_tool = None;
        }
    }

    /// Agent was cancelled (`AgentCancelled`).
    pub fn on_cancelled(&mut self, agent_name: &str) {
        if let Some(node) = self.nodes.get_mut(agent_name) {
            node.status = AgentStatus::Cancelled;
            node.status_changed_at = Instant::now();
        }
    }

    /// Reset all agent state (called when a new task starts).
    pub fn reset(&mut self) {
        self.order.clear();
        self.nodes.clear();
        self.run_started_at = None;
    }

    // ── Aggregate queries ─────────────────────────────────────────────────────

    /// Count of agents currently in an active status.
    pub fn active_count(&self) -> usize {
        self.nodes.values().filter(|n| n.status.is_active()).count()
    }

    /// Count of agents in Done status.
    pub fn done_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.status == AgentStatus::Done)
            .count()
    }

    /// Total elapsed since the first agent appeared, or zero if none yet.
    pub fn total_elapsed(&self) -> Duration {
        self.run_started_at.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// Agents in insertion order.
    pub fn agents_in_order(&self) -> impl Iterator<Item = &AgentNode> {
        self.order.iter().filter_map(|name| self.nodes.get(name))
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> AgentTracker {
        AgentTracker::new()
    }

    // 1. on_llm_start creates new agent in Thinking state
    #[test]
    fn llm_start_creates_agent_thinking() {
        let mut t = tracker();
        t.on_llm_start("coder");
        let node = t.nodes.get("coder").unwrap();
        assert_eq!(node.status, AgentStatus::Thinking);
        assert_eq!(node.turns, 1);
    }

    // 2. on_llm_start on existing agent updates status
    #[test]
    fn llm_start_updates_existing_agent() {
        let mut t = tracker();
        t.on_llm_start("coder");
        // Agent goes idle externally (simulate)
        t.nodes.get_mut("coder").unwrap().status = AgentStatus::Idle;
        t.on_llm_start("coder");
        let node = t.nodes.get("coder").unwrap();
        assert_eq!(node.status, AgentStatus::Thinking);
        assert_eq!(node.turns, 2);
    }

    // 3. on_tool_start transitions to ToolCalling
    #[test]
    fn tool_start_sets_tool_calling() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_tool_start("coder", "read_file", "tid-1", "path=src/main.rs");
        let node = t.nodes.get("coder").unwrap();
        assert_eq!(node.status, AgentStatus::ToolCalling);
        assert!(node.current_tool.is_some());
        let ct = node.current_tool.as_ref().unwrap();
        assert_eq!(ct.tool_name, "read_file");
        assert_eq!(ct.tool_use_id, "tid-1");
        assert!(ct.success.is_none());
    }

    // 4. on_tool_complete success → back to Thinking, increments completed
    #[test]
    fn tool_complete_success_back_to_thinking() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_tool_start("coder", "read_file", "tid-1", "");
        t.on_tool_complete("coder", "tid-1", true);
        let node = t.nodes.get("coder").unwrap();
        assert_eq!(node.status, AgentStatus::Thinking);
        assert_eq!(node.tool_calls_completed, 1);
        assert_eq!(node.tool_calls_failed, 0);
    }

    // 5. on_tool_complete failure → back to Thinking, increments failed
    #[test]
    fn tool_complete_failure_back_to_thinking() {
        let mut t = tracker();
        t.on_llm_start("qa");
        t.on_tool_start("qa", "bash_exec", "tid-2", "");
        t.on_tool_complete("qa", "tid-2", false);
        let node = t.nodes.get("qa").unwrap();
        assert_eq!(node.status, AgentStatus::Thinking);
        assert_eq!(node.tool_calls_failed, 1);
        assert_eq!(node.tool_calls_completed, 0);
    }

    // 6. on_approval_pending sets AwaitingApproval
    #[test]
    fn approval_pending_sets_status() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_approval_pending("coder");
        assert_eq!(
            t.nodes.get("coder").unwrap().status,
            AgentStatus::AwaitingApproval
        );
    }

    // 7. on_run_complete sets Done, clears current_tool
    #[test]
    fn run_complete_sets_done_clears_tool() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_tool_start("coder", "write_file", "tid-3", "");
        t.on_run_complete("coder");
        let node = t.nodes.get("coder").unwrap();
        assert_eq!(node.status, AgentStatus::Done);
        assert!(node.current_tool.is_none());
    }

    // 8. on_cancelled sets Cancelled
    #[test]
    fn cancelled_sets_status() {
        let mut t = tracker();
        t.on_llm_start("fixer");
        t.on_cancelled("fixer");
        assert_eq!(t.nodes.get("fixer").unwrap().status, AgentStatus::Cancelled);
    }

    // 9. reset clears all agents
    #[test]
    fn reset_clears_all() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_llm_start("qa");
        assert_eq!(t.nodes.len(), 2);
        t.reset();
        assert!(t.nodes.is_empty());
        assert!(t.order.is_empty());
        assert!(t.run_started_at.is_none());
    }

    // 10. active_count counts correctly
    #[test]
    fn active_count_correct() {
        let mut t = tracker();
        t.on_llm_start("coder"); // Thinking → active
        t.on_llm_start("qa"); // Thinking → active
        t.on_run_complete("qa"); // Done → not active
        assert_eq!(t.active_count(), 1);
    }

    // 11. done_count counts correctly
    #[test]
    fn done_count_correct() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_llm_start("qa");
        t.on_run_complete("coder");
        t.on_run_complete("qa");
        assert_eq!(t.done_count(), 2);
        assert_eq!(t.active_count(), 0);
    }

    // 12. Multiple agents tracked independently
    #[test]
    fn multiple_agents_independent() {
        let mut t = tracker();
        t.on_llm_start("planner");
        t.on_tool_start("planner", "read_file", "t1", "");
        t.on_llm_start("coder");
        t.on_tool_start("coder", "write_file", "t2", "");

        let planner = t.nodes.get("planner").unwrap();
        assert_eq!(planner.status, AgentStatus::ToolCalling);
        let coder = t.nodes.get("coder").unwrap();
        assert_eq!(coder.status, AgentStatus::ToolCalling);

        // Complete only coder's tool
        t.on_tool_complete("coder", "t2", true);

        assert_eq!(
            t.nodes.get("planner").unwrap().status,
            AgentStatus::ToolCalling
        );
        assert_eq!(t.nodes.get("coder").unwrap().status, AgentStatus::Thinking);
    }

    // 13. total_elapsed returns > 0 after first agent appears
    #[test]
    fn total_elapsed_positive_after_first_agent() {
        let mut t = tracker();
        t.on_llm_start("coder");
        assert!(t.total_elapsed() >= Duration::ZERO);
        assert!(t.run_started_at.is_some());
    }

    // 14. on_tool_complete for unknown agent doesn't panic
    #[test]
    fn tool_complete_unknown_agent_no_panic() {
        let mut t = tracker();
        // "ghost" was never created — should silently no-op
        t.on_tool_complete("ghost", "tid-x", true);
        assert!(t.nodes.is_empty());
    }

    // 15. tool_history capped at 20 entries
    #[test]
    fn tool_history_capped_at_20() {
        let mut t = tracker();
        t.on_llm_start("coder");
        for i in 0..25 {
            let id = format!("tid-{i}");
            t.on_tool_start("coder", "read_file", &id, "");
            t.on_tool_complete("coder", &id, true);
        }
        let node = t.nodes.get("coder").unwrap();
        assert!(
            node.tool_history.len() <= 20,
            "tool_history must be capped at 20, got {}",
            node.tool_history.len()
        );
    }

    // 16. on_llm_complete accumulates cost
    #[test]
    fn llm_complete_accumulates_cost() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_llm_complete("coder", 0.005);
        t.on_llm_complete("coder", 0.003);
        let node = t.nodes.get("coder").unwrap();
        assert!((node.cost_usd - 0.008).abs() < 1e-9);
    }

    // 17. on_llm_complete for unknown agent doesn't panic
    #[test]
    fn llm_complete_unknown_agent_no_panic() {
        let mut t = tracker();
        t.on_llm_complete("ghost", 0.005);
        // Should silently no-op
        assert!(t.nodes.is_empty());
    }

    // 18. agents_in_order preserves insertion order
    #[test]
    fn agents_in_order_preserved() {
        let mut t = tracker();
        t.on_llm_start("planner");
        t.on_llm_start("coder");
        t.on_llm_start("qa");
        let names: Vec<_> = t.agents_in_order().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["planner", "coder", "qa"]);
    }

    // 19. tool_complete marks history entry
    #[test]
    fn tool_complete_marks_history_entry() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.on_tool_start("coder", "read_file", "tid-1", "src/lib.rs");
        t.on_tool_complete("coder", "tid-1", true);
        let node = t.nodes.get("coder").unwrap();
        let entry = node
            .tool_history
            .iter()
            .find(|e| e.tool_use_id == "tid-1")
            .unwrap();
        assert_eq!(entry.success, Some(true));
        assert!(entry.finished_at.is_some());
    }

    // 20. status_duration is non-negative
    #[test]
    fn status_duration_non_negative() {
        let mut t = tracker();
        t.on_llm_start("coder");
        let node = t.nodes.get("coder").unwrap();
        assert!(node.status_duration() >= Duration::ZERO);
        assert!(node.total_duration() >= Duration::ZERO);
    }

    // 21. AgentStatus helpers work correctly
    #[test]
    fn agent_status_helpers() {
        assert!(AgentStatus::Thinking.is_active());
        assert!(AgentStatus::ToolCalling.is_active());
        assert!(AgentStatus::AwaitingApproval.is_active());
        assert!(!AgentStatus::Idle.is_active());
        assert!(!AgentStatus::Done.is_active());
        assert!(!AgentStatus::Failed.is_active());
        assert!(!AgentStatus::Cancelled.is_active());

        assert!(!AgentStatus::Thinking.icon().is_empty());
        assert!(!AgentStatus::Done.label().is_empty());
    }

    // 22. ToolCallInfo elapsed_str is reasonable
    #[test]
    fn tool_call_elapsed_str_reasonable() {
        let info = ToolCallInfo {
            tool_name: "read_file".into(),
            input_summary: "path=foo".into(),
            tool_use_id: "t1".into(),
            started_at: Instant::now(),
            finished_at: None,
            success: None,
        };
        let s = info.elapsed_str();
        assert!(s.ends_with("ms") || s.ends_with('s'));
    }

    // 23. reset followed by new agents works
    #[test]
    fn reset_then_new_agents() {
        let mut t = tracker();
        t.on_llm_start("coder");
        t.reset();
        t.on_llm_start("qa");
        assert_eq!(t.nodes.len(), 1);
        assert!(t.nodes.contains_key("qa"));
    }

    // 24. on_approval_pending for unknown agent doesn't panic
    #[test]
    fn approval_pending_unknown_agent_no_panic() {
        let mut t = tracker();
        t.on_approval_pending("ghost");
        assert!(t.nodes.is_empty());
    }

    // 25. on_run_complete for unknown agent doesn't panic
    #[test]
    fn run_complete_unknown_agent_no_panic() {
        let mut t = tracker();
        t.on_run_complete("ghost");
        assert!(t.nodes.is_empty());
    }
}
