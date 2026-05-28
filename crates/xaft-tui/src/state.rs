//! Application state — the single source of truth for the TUI.
//!
//! `AppState` is updated by `TuiEvent`s and produces `RenderMutation`s that
//! the main event loop applies to the `IncrementalRenderer`.
//! All mutations happen in the main event loop (single-threaded); no locking needed.

use std::collections::HashMap;
use std::time::Instant;

use xaft_runtime::session::{AgentSession, SessionStatus};

use crate::agent_tracker::AgentTracker;
use crate::approval::{ApprovalDecision, ApprovalQueue, AutoApproveConfig};
use crate::bridge::TuiEvent;
use crate::prompt::build_prompt;
use crate::transcript::{
    LineKind, RenderMutation, StyledLine, build_file_diff_lines, format_tool_call_inline,
    input_preview, to_pascal_case,
};

// ── AppState ──────────────────────────────────────────────────────────────────

/// Full state of the TUI application.
#[derive(Debug)]
pub struct AppState {
    // ── Runtime state ─────────────────────────────────────────────────────────
    pub session: Option<AgentSession>,
    pub task: String,
    pub phase: WorkflowPhase,
    pub task_done: bool,
    pub final_summary: String,

    // ── Token / cost stats ─────────────────────────────────────────────────────
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub total_llm_calls: u32,
    pub current_agent: String,
    pub current_agent_turns: usize,
    pub agent_costs: HashMap<String, f64>,
    pub agent_tokens: HashMap<String, u64>,

    // ── Approval ──────────────────────────────────────────────────────────────
    pub approval_queue: ApprovalQueue,
    pub pending_gate_decisions: Vec<(String, bool)>,

    // ── Inline diff ───────────────────────────────────────────────────────────
    pub pending_file_inputs: HashMap<String, serde_json::Value>,

    // ── Input bar ─────────────────────────────────────────────────────────────
    pub input_buffer: String,
    pub user_message_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,

    // ── Agent activity tracker ────────────────────────────────────────────────
    pub agent_tracker: AgentTracker,

    // ── Timestamps ────────────────────────────────────────────────────────────
    pub agent_start_time: Option<Instant>,
    pub task_start_time: Option<Instant>,
    pub session_start_time: Option<Instant>,

    // ── Session / git ─────────────────────────────────────────────────────────
    pub last_commit_sha: Option<String>,
    pub git_branch: Option<String>,

    // ── Misc ──────────────────────────────────────────────────────────────────
    pub should_quit: bool,
    pub error_message: Option<String>,
    pub terminal_size: (u16, u16),

    // ── Graceful shutdown ─────────────────────────────────────────────────────
    pub first_ctrl_c_at: Option<Instant>,
    pub cancel_requested: bool,

    // ── Streaming / render ────────────────────────────────────────────────────
    /// True while a stream line is open (tracking for logic, not for rendering).
    pub stream_active: bool,
    /// Spinner animation tick (incremented by app loop at ~10fps).
    pub spinner_tick: u64,

    // ── Render mutations ──────────────────────────────────────────────────────
    /// Mutations produced by `handle_event`; drained by the app loop.
    pub mutations: Vec<RenderMutation>,
}

// ── Sub-types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowPhase {
    Idle,
    Planning,
    Coding,
    QaReview,
    Fixing,
    Done,
    Error,
}

impl WorkflowPhase {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Planning => "Planning",
            Self::Coding => "Coder",
            Self::QaReview => "QA Review",
            Self::Fixing => "Fixer",
            Self::Done => "Done",
            Self::Error => "Error",
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Idle | Self::Done | Self::Error)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

impl AppState {
    pub fn new(task: impl Into<String>) -> Self {
        let task = task.into();
        let phase = if task.trim().is_empty() {
            WorkflowPhase::Idle
        } else {
            WorkflowPhase::Planning
        };
        Self {
            session: None,
            task,
            phase,
            task_done: false,
            final_summary: String::new(),

            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            total_llm_calls: 0,
            current_agent: String::new(),
            current_agent_turns: 0,
            agent_costs: HashMap::new(),
            agent_tokens: HashMap::new(),

            approval_queue: ApprovalQueue::new(AutoApproveConfig::default_safe()),
            pending_gate_decisions: Vec::new(),

            pending_file_inputs: HashMap::new(),

            input_buffer: String::new(),
            user_message_tx: None,

            agent_tracker: AgentTracker::new(),

            agent_start_time: None,
            task_start_time: None,
            session_start_time: None,

            last_commit_sha: None,
            git_branch: None,

            should_quit: false,
            error_message: None,
            terminal_size: (120, 40),

            first_ctrl_c_at: None,
            cancel_requested: false,

            stream_active: false,
            spinner_tick: 0,

            mutations: Vec::new(),
        }
    }

    /// Reset streaming and agent state for a new task.
    pub fn reset_for_new_task(&mut self) {
        self.agent_tracker.reset();
        self.stream_active = false;
        self.agent_start_time = None;
        self.task_start_time = None;
        self.pending_file_inputs.clear();
        self.phase = WorkflowPhase::Idle;
        self.current_agent.clear();
        self.current_agent_turns = 0;
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cost_usd = 0.0;
        self.total_llm_calls = 0;
        self.agent_costs.clear();
        self.agent_tokens.clear();
    }

    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    pub fn top_agents_by_cost(&self) -> Vec<(&str, f64)> {
        let mut v: Vec<(&str, f64)> = self
            .agent_costs
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(5);
        v
    }

    // ── Event handling ────────────────────────────────────────────────────────

    /// Handle a `TuiEvent` — the single mutation point for all state changes.
    ///
    /// Appends `RenderMutation` items to `self.mutations` for the app loop to apply.
    pub fn handle_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Key(key) => self.handle_key(key),

            TuiEvent::Resize(w, h) => {
                self.terminal_size = (w, h);
                self.mutations
                    .push(RenderMutation::Resize { cols: w, rows: h });
            }

            TuiEvent::LlmCallStarting { agent_name, .. } => {
                let agent_changed = self.current_agent != agent_name;
                self.current_agent = agent_name.clone();
                self.phase = infer_phase_from_agent(&agent_name);
                self.stream_active = true;

                if self.agent_start_time.is_none() {
                    self.agent_start_time = Some(Instant::now());
                }

                if agent_changed {
                    let icon = match agent_name.as_str() {
                        "planner" | "summary" => "◈",
                        "coder" => "◉",
                        "qa" => "◎",
                        "fixer" => "◌",
                        _ => "◆",
                    };
                    if self.stream_active {
                        self.mutations.push(RenderMutation::FlushStream);
                        self.stream_active = false;
                    }
                    let display_name = capitalize_first(&agent_name);
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("{icon} {display_name}"),
                            LineKind::AgentMarker,
                        )));
                }
                self.agent_tracker.on_llm_start(&agent_name);
            }

            TuiEvent::LlmCallComplete {
                agent_name,
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                let tokens = (input_tokens + output_tokens) as u64;
                self.total_input_tokens += input_tokens as u64;
                self.total_output_tokens += output_tokens as u64;
                self.total_cost_usd += cost_usd;
                self.total_llm_calls += 1;
                *self.agent_costs.entry(agent_name.clone()).or_insert(0.0) += cost_usd;
                *self.agent_tokens.entry(agent_name.clone()).or_insert(0) += tokens;
                self.agent_tracker.on_llm_complete(&agent_name, cost_usd);
            }

            TuiEvent::StreamToken { agent_name, token } => {
                if self.current_agent != agent_name {
                    self.current_agent = agent_name;
                }
                self.stream_active = true;
                self.mutations.push(RenderMutation::StreamToken {
                    fragment: token,
                    style: crate::transcript::LineStyle::Dim,
                });
            }

            TuiEvent::AgentOutput {
                agent_name,
                content,
            } => {
                self.current_agent = agent_name.clone();
                // Flush any open stream line — AgentOutput is the authoritative content.
                if self.stream_active {
                    self.mutations.push(RenderMutation::FlushStream);
                    self.stream_active = false;
                }
                // Commit each non-empty line permanently, indented 2 spaces.
                for line in content.lines() {
                    if !line.trim().is_empty() {
                        self.mutations.push(RenderMutation::CommitLine(
                            StyledLine::new(format!("  {line}"), LineKind::AgentText)
                                .with_agent(&agent_name),
                        ));
                    }
                }
            }

            TuiEvent::AgentRunComplete {
                agent_name,
                turns,
                total_cost_usd,
            } => {
                self.stream_active = false;
                self.current_agent_turns += turns;
                self.total_cost_usd = total_cost_usd.max(self.total_cost_usd);
                if self.stream_active {
                    self.mutations.push(RenderMutation::FlushStream);
                }
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("    done ({turns} turns)"),
                        LineKind::System,
                    )));
                self.agent_tracker.on_run_complete(&agent_name);
            }

            TuiEvent::AgentCancelled { agent_name, reason } => {
                self.phase = WorkflowPhase::Error;
                self.stream_active = false;
                self.mutations.push(RenderMutation::FlushStream);
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("[{agent_name}] Cancelled: {reason}"),
                        LineKind::Error,
                    )));
                self.agent_tracker.on_cancelled(&agent_name);
            }

            TuiEvent::ToolStarted {
                tool_name,
                tool_use_id,
                input,
                ..
            } => {
                let preview = input_preview(&input, 60);

                // Flush streaming before tool call line.
                if self.stream_active {
                    self.mutations.push(RenderMutation::FlushStream);
                    self.stream_active = false;
                }

                let call_str = format_tool_call_inline(&tool_name, &input, 60);
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("  {call_str}"),
                        LineKind::ToolCall,
                    )));

                // Transient reading hint for read-only tools.
                if matches!(tool_name.as_str(), "read_file" | "list_files" | "grep") {
                    let hint = match tool_name.as_str() {
                        "read_file" => {
                            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("file");
                            let fname = std::path::Path::new(path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(path);
                            format!("  Reading {}…", fname)
                        }
                        "list_files" => "  Listing files…".to_string(),
                        "grep" => {
                            let pat = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
                            format!("  Searching for '{}'…", pat)
                        }
                        _ => "  Reading…".to_string(),
                    };
                    self.mutations.push(RenderMutation::StreamToken {
                        fragment: hint,
                        style: crate::transcript::LineStyle::Dim,
                    });
                    self.stream_active = true;
                }

                if tool_name == "edit_file" || tool_name == "write_file" {
                    self.pending_file_inputs
                        .insert(tool_use_id.clone(), input.clone());
                }

                let agent = self.current_agent.clone();
                self.agent_tracker
                    .on_tool_start(&agent, &tool_name, &tool_use_id, &preview);
            }

            TuiEvent::ToolCompleted {
                tool_name,
                tool_use_id,
                duration_ms,
                success,
                error,
            } => {
                // Clear any transient reading hint.
                if self.stream_active {
                    self.mutations.push(RenderMutation::FlushStream);
                    self.stream_active = false;
                }

                let file_diff_input = if success {
                    self.pending_file_inputs.remove(&tool_use_id)
                } else {
                    self.pending_file_inputs.remove(&tool_use_id);
                    None
                };

                if success {
                    let dur_str = format_duration_ms(duration_ms);
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("    ✓ {dur_str}"),
                            LineKind::ToolResult,
                        )));
                    if let Some(file_input) = file_diff_input {
                        let (cols, _) = self.terminal_size;
                        for line in build_file_diff_lines(&tool_name, &file_input, cols) {
                            self.mutations.push(RenderMutation::CommitLine(line));
                        }
                    }
                } else if let Some(err) = error {
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("  [{tool_name}] FAILED"),
                            LineKind::Error,
                        )));
                    for line in err.lines() {
                        if !line.trim().is_empty() {
                            self.mutations
                                .push(RenderMutation::CommitLine(StyledLine::new(
                                    format!("  {line}"),
                                    LineKind::Error,
                                )));
                        }
                    }
                }

                let agent = self.current_agent.clone();
                self.agent_tracker
                    .on_tool_complete(&agent, &tool_use_id, success);
            }

            TuiEvent::ToolPendingApproval {
                agent_run_id,
                tool_name,
                tool_use_id,
                input,
                ..
            } => {
                let tool_name_clone = tool_name.clone();
                let result =
                    self.approval_queue
                        .push(tool_use_id.clone(), agent_run_id, tool_name, input);
                match result {
                    Some(decision) => {
                        self.pending_gate_decisions
                            .push((tool_use_id, decision.is_approved()));
                    }
                    None => {
                        let pascal = to_pascal_case(&tool_name_clone);
                        // Clear any stream/hint before the approval prompt.
                        if self.stream_active {
                            self.mutations.push(RenderMutation::FlushStream);
                            self.stream_active = false;
                        }
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!("  ⚠ Approve {pascal}? [a]yes  [r]no  [s]skip"),
                                LineKind::System,
                            )));
                        let agent = self.current_agent.clone();
                        self.agent_tracker.on_approval_pending(&agent);
                    }
                }
            }

            TuiEvent::CommitCreated {
                agent_name,
                short_sha,
                message,
                files_changed,
            } => {
                self.last_commit_sha = Some(short_sha.clone());
                self.git_branch = self.session.as_ref().and_then(|s| s.git_branch.clone());
                self.mutations.push(RenderMutation::CommitLine(
                    StyledLine::new(
                        format!("✓ Committed [{short_sha}]: {message} ({files_changed} files)"),
                        LineKind::Success,
                    )
                    .with_agent(&agent_name),
                ));
            }

            TuiEvent::FileEditsCommitted {
                files,
                lines_added,
                lines_removed,
                ..
            } => {
                if lines_added > 0 || lines_removed > 0 {
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!(
                                "Edited {} file(s): +{lines_added}/−{lines_removed} lines",
                                files.len()
                            ),
                            LineKind::System,
                        )));
                }
            }

            TuiEvent::SessionUpdate(session) => {
                if let Some(ref b) = session.git_branch {
                    self.git_branch = Some(b.clone());
                }
                match &session.status {
                    SessionStatus::Completed { summary } => {
                        self.phase = WorkflowPhase::Done;
                        self.task_done = true;
                        self.final_summary = summary.clone();
                        let s = summary.clone();
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!("✓ Task complete: {s}"),
                                LineKind::Success,
                            )));
                    }
                    SessionStatus::Failed { error } => {
                        self.phase = WorkflowPhase::Error;
                        let e = error.clone();
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!("✗ Task failed: {e}"),
                                LineKind::Error,
                            )));
                    }
                    SessionStatus::Cancelled => {
                        self.phase = WorkflowPhase::Error;
                    }
                    _ => {}
                }
                self.session = Some(session);
            }

            TuiEvent::TaskComplete { summary } => {
                self.phase = WorkflowPhase::Done;
                self.task_done = true;
                self.final_summary = summary;
                // Flush any open stream.
                if self.stream_active {
                    self.mutations.push(RenderMutation::FlushStream);
                    self.stream_active = false;
                }
                self.mutations.push(RenderMutation::SetEphemeral(None));
                self.agent_tracker.reset();
            }

            TuiEvent::AgentHandoff {
                from_agent,
                to_agent,
                ..
            } => {
                if self.stream_active {
                    self.mutations.push(RenderMutation::FlushStream);
                    self.stream_active = false;
                }
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("  ↝ {from_agent} → {to_agent}"),
                        LineKind::AgentMarker,
                    )));
            }

            TuiEvent::RuntimeError(msg) => {
                self.phase = WorkflowPhase::Error;
                self.error_message = Some(msg.clone());
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("✗ {msg}"),
                        LineKind::Error,
                    )));
            }

            TuiEvent::Mouse(_) => {
                // Mouse events not used in conversational mode.
            }
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        self.error_message = None;

        // Inline approval keys take priority over the input buffer.
        // When the agent is blocked waiting for approval, a/r/s resolve it.
        if self.approval_queue.has_pending() {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(id) = self
                        .approval_queue
                        .resolve_focused(ApprovalDecision::Approved)
                    {
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                "  ✓ Approved".to_string(),
                                LineKind::Success,
                            )));
                        self.pending_gate_decisions.push((id, true));
                    }
                    return;
                }
                KeyCode::Char('r') | KeyCode::Char('n') | KeyCode::Char('N') => {
                    if let Some(id) = self
                        .approval_queue
                        .resolve_focused(ApprovalDecision::Rejected)
                    {
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                "  ✗ Rejected".to_string(),
                                LineKind::Error,
                            )));
                        self.pending_gate_decisions.push((id, false));
                    }
                    return;
                }
                KeyCode::Char('s') => {
                    self.approval_queue.focus_next();
                    return;
                }
                _ => {}
            }
        }

        // InputBar: capture printable chars and control sequences.
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input_buffer.push(c);
                let prompt = build_prompt(self);
                self.mutations.push(RenderMutation::UpdatePrompt(prompt));
                return;
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                let prompt = build_prompt(self);
                self.mutations.push(RenderMutation::UpdatePrompt(prompt));
                return;
            }
            KeyCode::Enter => {
                let msg = self.input_buffer.trim().to_string();
                if !msg.is_empty() {
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("❯ {msg}"),
                            LineKind::UserMessage,
                        )));
                    if let Some(ref tx) = self.user_message_tx {
                        let _ = tx.send(msg.clone());
                    }
                    self.task = msg;
                    self.task_start_time = Some(Instant::now());
                    self.input_buffer.clear();
                    let prompt = build_prompt(self);
                    self.mutations.push(RenderMutation::UpdatePrompt(prompt));
                }
                return;
            }
            _ => {}
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || self.task_done
                    || self.phase == WorkflowPhase::Error
                {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.cancel_requested {
                    self.should_quit = true;
                } else {
                    self.cancel_requested = true;
                    self.first_ctrl_c_at = Some(Instant::now());
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            "  Press Ctrl+C again to force exit".to_string(),
                            LineKind::System,
                        )));
                }
            }
            _ => {}
        }
    }

    /// Emit a visual separator between tasks.
    pub fn push_separator(&mut self) {
        self.mutations
            .push(RenderMutation::CommitLine(StyledLine::new(
                "─".repeat(60),
                LineKind::Separator,
            )));
    }

    /// Check if the Ctrl+C timeout has elapsed and trigger quit.
    pub fn check_ctrl_c_timeout(&mut self) {
        if self.cancel_requested {
            if let Some(at) = self.first_ctrl_c_at {
                if at.elapsed() > std::time::Duration::from_secs(2) {
                    self.should_quit = true;
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn infer_phase_from_agent(name: &str) -> WorkflowPhase {
    match name {
        "planner" | "summary" => WorkflowPhase::Planning,
        "coder" => WorkflowPhase::Coding,
        "qa" => WorkflowPhase::QaReview,
        "fixer" => WorkflowPhase::Fixing,
        _ => WorkflowPhase::Coding,
    }
}

fn format_duration_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1}s", ms / 1000.0)
    } else {
        format!("{:.0}ms", ms)
    }
}

/// Format a duration for the elapsed-time indicator.
pub fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Compact token count: "1.2k", "3.4M", plain number under 1000.
pub fn format_tokens_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ── Helper to extract CommitLine texts from mutations (test utility) ──────────

/// Extract text from all `CommitLine` mutations in a list.
pub fn commit_line_texts(mutations: &[RenderMutation]) -> Vec<&str> {
    mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.as_str()),
            _ => None,
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::RiskLevel;

    fn make_state() -> AppState {
        AppState::new("test task")
    }

    fn commit_texts(s: &AppState) -> Vec<&str> {
        commit_line_texts(&s.mutations)
    }

    #[test]
    fn initial_state() {
        let s = make_state();
        assert_eq!(s.task, "test task");
        assert_eq!(s.phase, WorkflowPhase::Planning);
        assert!(!s.should_quit);
        assert!(s.mutations.is_empty());
        assert_eq!(s.total_tokens(), 0);
    }

    #[test]
    fn initial_phase_planning_for_non_empty() {
        let s = AppState::new("fix the bug");
        assert_eq!(s.phase, WorkflowPhase::Planning);
    }

    #[test]
    fn initial_phase_idle_for_empty() {
        let s = AppState::new("");
        assert_eq!(s.phase, WorkflowPhase::Idle);
        let s2 = AppState::new("   ");
        assert_eq!(s2.phase, WorkflowPhase::Idle);
    }

    #[test]
    fn agent_output_commits_lines() {
        let mut s = make_state();
        s.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "Hello thinking output\nSecond line here".into(),
        });
        let texts = commit_texts(&s);
        assert!(
            texts.iter().any(|t| t.contains("Hello thinking output")),
            "AgentOutput must produce CommitLine mutations: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Second line here")),
            "second line: {texts:?}"
        );
    }

    #[test]
    fn tool_started_commits_tool_call_line() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "read_file".into(),
            tool_use_id: "tid-1".into(),
            input: serde_json::json!({"path": "test.py"}),
            started_at: Instant::now(),
        });
        let texts = commit_texts(&s);
        assert!(
            texts.iter().any(|t| t.contains("ReadFile")),
            "ToolStarted must produce tool call line: {texts:?}"
        );
    }

    #[test]
    fn tool_completed_success_commits_result_line() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "write_file".into(),
            tool_use_id: "tid-2".into(),
            input: serde_json::json!({}),
            started_at: Instant::now(),
        });
        s.mutations.clear();
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "write_file".into(),
            tool_use_id: "tid-2".into(),
            duration_ms: 50.0,
            success: true,
            error: None,
        });
        let texts = commit_texts(&s);
        assert!(
            texts.iter().any(|t| t.contains("✓")),
            "ToolCompleted success must commit ✓ line: {texts:?}"
        );
    }

    #[test]
    fn pending_approval_high_risk_produces_inline_prompt() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolPendingApproval {
            agent_run_id: "run-1".into(),
            tool_name: "bash_exec".into(),
            tool_use_id: "tid-3".into(),
            input: serde_json::json!({"command": "rm -rf /tmp/test"}),
            risk: RiskLevel::High,
        });
        assert!(s.approval_queue.has_pending());
        let texts = commit_texts(&s);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("⚠") && t.contains("BashExec")),
            "must have inline ⚠ approval prompt: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("[a]yes") && t.contains("[r]no")),
            "must have key hints: {texts:?}"
        );
    }

    #[test]
    fn low_risk_tool_auto_approved() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolPendingApproval {
            agent_run_id: "run-2".into(),
            tool_name: "read_file".into(),
            tool_use_id: "tid-auto".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
            risk: RiskLevel::Low,
        });
        assert!(!s.approval_queue.has_pending());
        assert_eq!(s.pending_gate_decisions.len(), 1);
        assert!(s.pending_gate_decisions[0].1); // approved = true
    }

    #[test]
    fn runtime_error_sets_phase() {
        let mut s = make_state();
        s.handle_event(TuiEvent::RuntimeError("boom".into()));
        assert_eq!(s.phase, WorkflowPhase::Error);
        assert!(s.error_message.is_some());
        let texts = commit_texts(&s);
        assert!(texts.iter().any(|t| t.contains("✗") && t.contains("boom")));
    }

    #[test]
    fn token_stats_accumulate() {
        let mut s = make_state();
        s.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 100,
            output_tokens: 200,
            cost_usd: 0.01,
            duration_ms: 500.0,
        });
        s.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "qa".into(),
            input_tokens: 50,
            output_tokens: 100,
            cost_usd: 0.005,
            duration_ms: 300.0,
        });
        assert_eq!(s.total_input_tokens, 150);
        assert_eq!(s.total_output_tokens, 300);
        assert_eq!(s.total_tokens(), 450);
        assert!((s.total_cost_usd - 0.015).abs() < 1e-9);
    }

    #[test]
    fn stream_token_sets_stream_active() {
        let mut s = make_state();
        s.handle_event(TuiEvent::StreamToken {
            agent_name: "coder".into(),
            token: "hello".into(),
        });
        assert!(s.stream_active);
        let stream_muts: Vec<_> = s
            .mutations
            .iter()
            .filter(|m| matches!(m, RenderMutation::StreamToken { .. }))
            .collect();
        assert_eq!(stream_muts.len(), 1);
    }

    #[test]
    fn agent_output_flushes_stream_first() {
        let mut s = make_state();
        s.handle_event(TuiEvent::StreamToken {
            agent_name: "coder".into(),
            token: "partial".into(),
        });
        s.mutations.clear();
        s.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "Full response".into(),
        });
        // First mutation should be FlushStream
        assert!(matches!(
            s.mutations.first(),
            Some(RenderMutation::FlushStream)
        ));
    }

    #[test]
    fn llm_call_starting_different_agent_emits_marker() {
        let mut s = make_state();
        s.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "planner".into(),
            call_index: 0,
        });
        let texts = commit_texts(&s);
        assert!(
            texts.iter().any(|t| t.contains("planner")),
            "must emit agent marker: {texts:?}"
        );
    }

    #[test]
    fn commit_created_emits_success_line() {
        let mut s = make_state();
        s.handle_event(TuiEvent::CommitCreated {
            agent_name: "coder".into(),
            short_sha: "abc1234".into(),
            message: "fix bug".into(),
            files_changed: 2,
        });
        let texts = commit_texts(&s);
        assert!(texts.iter().any(|t| t.contains("abc1234")));
    }

    #[test]
    fn task_complete_clears_stream_and_ephemeral() {
        let mut s = make_state();
        s.stream_active = true;
        s.handle_event(TuiEvent::TaskComplete {
            summary: "done".into(),
        });
        assert!(!s.stream_active);
        assert!(s.task_done);
        assert!(
            s.mutations
                .iter()
                .any(|m| matches!(m, RenderMutation::FlushStream))
        );
        assert!(
            s.mutations
                .iter()
                .any(|m| matches!(m, RenderMutation::SetEphemeral(None)))
        );
    }

    #[test]
    fn ctrl_c_once_sets_cancel_requested() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert!(s.cancel_requested);
        assert!(!s.should_quit);
    }

    #[test]
    fn ctrl_c_twice_sets_should_quit() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        let key = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        s.handle_event(TuiEvent::Key(key.clone()));
        s.handle_event(TuiEvent::Key(key));
        assert!(s.should_quit);
    }

    #[test]
    fn enter_with_text_commits_user_message() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        s.input_buffer = "hello world".into();
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        let texts = commit_texts(&s);
        assert!(texts.iter().any(|t| t.contains("hello world")));
        assert!(
            s.input_buffer.is_empty(),
            "buffer should be cleared after enter"
        );
    }

    #[test]
    fn backspace_pops_input_and_updates_prompt() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        s.input_buffer = "hello".into();
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert_eq!(s.input_buffer, "hell");
        assert!(
            s.mutations
                .iter()
                .any(|m| matches!(m, RenderMutation::UpdatePrompt(_)))
        );
    }

    #[test]
    fn agent_handoff_commits_line() {
        let mut s = make_state();
        s.handle_event(TuiEvent::AgentHandoff {
            from_agent: "planner".into(),
            to_agent: "coder".into(),
            summary: "here is the plan".into(),
        });
        let texts = commit_texts(&s);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("planner") && t.contains("coder"))
        );
    }

    #[test]
    fn resize_produces_resize_mutation() {
        let mut s = make_state();
        s.handle_event(TuiEvent::Resize(100, 30));
        assert_eq!(s.terminal_size, (100, 30));
        assert!(s.mutations.iter().any(|m| matches!(
            m,
            RenderMutation::Resize {
                cols: 100,
                rows: 30
            }
        )));
    }

    #[test]
    fn inline_diff_edit_file_produces_commit_lines() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-edit".into(),
            input: serde_json::json!({
                "path": "src/main.py",
                "old_content": "import random\n",
                "new_content": "import secrets\n"
            }),
            started_at: Instant::now(),
        });
        s.mutations.clear();
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-edit".into(),
            duration_ms: 8.0,
            success: true,
            error: None,
        });
        assert!(
            !s.pending_file_inputs.contains_key("tid-edit"),
            "input must be consumed"
        );
        let texts = commit_texts(&s);
        let has_summary = texts.iter().any(|t| t.contains("⎿"));
        assert!(has_summary, "must have ⎿ summary: {texts:?}");
    }

    #[test]
    fn inline_diff_not_shown_on_failure() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-fail".into(),
            input: serde_json::json!({
                "path": "src/x.py",
                "old_content": "old\n",
                "new_content": "new\n"
            }),
            started_at: Instant::now(),
        });
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-fail".into(),
            duration_ms: 3.0,
            success: false,
            error: Some("pattern not found".into()),
        });
        assert!(
            !s.pending_file_inputs.contains_key("tid-fail"),
            "input must be cleaned up"
        );
        let texts = commit_texts(&s);
        assert!(!texts.iter().any(|t| t.contains("⎿")), "no diff on failure");
    }

    #[test]
    fn format_elapsed_seconds() {
        assert_eq!(format_elapsed(std::time::Duration::from_secs(45)), "45s");
    }

    #[test]
    fn format_elapsed_minutes() {
        assert_eq!(format_elapsed(std::time::Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn format_tokens_compact_k() {
        assert_eq!(format_tokens_compact(8_600), "8.6k");
    }

    #[test]
    fn format_tokens_compact_m() {
        assert_eq!(format_tokens_compact(1_200_000), "1.2M");
    }

    #[test]
    fn format_tokens_compact_small() {
        assert_eq!(format_tokens_compact(42), "42");
    }
}
