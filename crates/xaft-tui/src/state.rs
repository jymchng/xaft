//! Application state — the single source of truth for the TUI.
//!
//! `AppState` is updated by `TuiEvent`s and read by the renderer.
//! All mutations happen in the main event loop (single-threaded); no locking needed.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use xaft_runtime::session::{AgentSession, SessionStatus};

use crate::bridge::TuiEvent;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum output lines retained in the conversation buffer.
pub const MAX_OUTPUT_LINES: usize = 2000;

/// Maximum tool log entries retained.
pub const MAX_TOOL_ENTRIES: usize = 200;

/// Maximum log entries retained.
pub const MAX_LOG_ENTRIES: usize = 500;

// ── AppState ──────────────────────────────────────────────────────────────────

/// Full state of the TUI application.
#[derive(Debug)]
pub struct AppState {
    // ── Runtime state ─────────────────────────────────────────────────────────
    /// Current session (set when task starts).
    pub session: Option<AgentSession>,
    /// Task string given by user.
    pub task: String,
    /// Current workflow phase.
    pub phase: WorkflowPhase,
    /// Whether the task has finished.
    pub task_done: bool,
    /// Final summary on completion.
    pub final_summary: String,

    // ── Conversation output ────────────────────────────────────────────────────
    /// Accumulated output lines (agent text, tool results, system messages).
    pub output_lines: VecDeque<OutputLine>,
    /// Scroll offset (lines from bottom, 0 = bottom).
    pub output_scroll: usize,
    /// Whether output is auto-scrolling to bottom.
    pub output_auto_scroll: bool,

    // ── Tool activity ─────────────────────────────────────────────────────────
    /// Recent tool calls (newest last).
    pub tool_log: VecDeque<ToolEntry>,
    /// Currently executing tool (if any).
    pub active_tool: Option<ActiveTool>,

    // ── Token / cost stats ─────────────────────────────────────────────────────
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub total_llm_calls: u32,
    pub current_agent: String,
    pub current_agent_turns: usize,

    // ── Approval ──────────────────────────────────────────────────────────────
    /// Pending approval request, if any.
    pub pending_approval: Option<PendingApprovalState>,
    /// Which button is focused in the approval dialog.
    pub approval_focused_approve: bool,

    // ── UI focus / navigation ─────────────────────────────────────────────────
    pub focused_panel: FocusedPanel,

    // ── Misc ──────────────────────────────────────────────────────────────────
    /// System/debug log entries.
    pub log: VecDeque<LogEntry>,
    /// Whether to show the debug log panel.
    pub show_log: bool,
    /// Tick counter (incremented 60×/s).
    pub tick: u64,
    /// Whether the TUI should quit.
    pub should_quit: bool,
    /// Last error message (cleared on next keypress).
    pub error_message: Option<String>,
    /// Git branch created for this run.
    pub git_branch: Option<String>,
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

#[derive(Debug, Clone)]
pub struct OutputLine {
    pub kind: OutputKind,
    pub text: String,
    pub agent: Option<String>,
    pub timestamp: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputKind {
    AgentText,
    ToolResult,
    System,
    Error,
    Success,
}

#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub name: String,
    pub tool_use_id: String,
    pub input_preview: String,
    pub state: ToolEntryState,
    pub started_at: Instant,
    pub duration_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEntryState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ActiveTool {
    pub name: String,
    pub tool_use_id: String,
    pub started_at: Instant,
}

#[derive(Debug, Clone)]
pub struct PendingApprovalState {
    pub agent_run_id: String,
    pub tool_name: String,
    pub tool_use_id: String,
    pub input: serde_json::Value,
    pub input_preview: String,
    pub arrived_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedPanel {
    Conversation,
    ToolLog,
    Approval,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    pub timestamp: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

// ── Implementation ────────────────────────────────────────────────────────────

impl AppState {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            session: None,
            task: task.into(),
            phase: WorkflowPhase::Planning,
            task_done: false,
            final_summary: String::new(),

            output_lines: VecDeque::new(),
            output_scroll: 0,
            output_auto_scroll: true,

            tool_log: VecDeque::new(),
            active_tool: None,

            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            total_llm_calls: 0,
            current_agent: String::new(),
            current_agent_turns: 0,

            pending_approval: None,
            approval_focused_approve: true,

            focused_panel: FocusedPanel::Conversation,

            log: VecDeque::new(),
            show_log: false,
            tick: 0,
            should_quit: false,
            error_message: None,
            git_branch: None,
        }
    }

    /// Handle a `TuiEvent` — the single mutation point for all state changes.
    pub fn handle_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
            }

            TuiEvent::Key(key) => self.handle_key(key),

            TuiEvent::Resize(_, _) => {} // ratatui handles resize automatically

            TuiEvent::LlmCallStarting { agent_name, .. } => {
                self.current_agent = agent_name.clone();
                self.phase = infer_phase_from_agent(&agent_name);
                self.log_info(format!("[{agent_name}] thinking…"));
            }

            TuiEvent::LlmCallComplete {
                agent_name,
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                self.total_input_tokens += input_tokens as u64;
                self.total_output_tokens += output_tokens as u64;
                self.total_cost_usd += cost_usd;
                self.total_llm_calls += 1;
                self.log_info(format!(
                    "[{agent_name}] LLM call: {input_tokens}+{output_tokens} tokens"
                ));
            }

            TuiEvent::AgentOutput {
                agent_name,
                content,
            } => {
                self.current_agent = agent_name.clone();
                self.push_output(OutputLine {
                    kind: OutputKind::AgentText,
                    text: content,
                    agent: Some(agent_name),
                    timestamp: Instant::now(),
                });
            }

            TuiEvent::AgentRunComplete {
                agent_name,
                turns,
                total_cost_usd,
            } => {
                self.current_agent_turns += turns;
                self.total_cost_usd = total_cost_usd.max(self.total_cost_usd);
                self.log_info(format!("[{agent_name}] run complete ({turns} turns)"));
            }

            TuiEvent::AgentCancelled { agent_name, reason } => {
                self.phase = WorkflowPhase::Error;
                self.push_output(OutputLine {
                    kind: OutputKind::Error,
                    text: format!("[{agent_name}] Cancelled: {reason}"),
                    agent: Some(agent_name),
                    timestamp: Instant::now(),
                });
            }

            TuiEvent::ToolStarted {
                tool_name,
                tool_use_id,
                input,
                started_at,
            } => {
                let preview = input_preview(&input, 60);
                self.active_tool = Some(ActiveTool {
                    name: tool_name.clone(),
                    tool_use_id: tool_use_id.clone(),
                    started_at,
                });
                self.tool_log.push_back(ToolEntry {
                    name: tool_name,
                    tool_use_id,
                    input_preview: preview,
                    state: ToolEntryState::Running,
                    started_at,
                    duration_ms: None,
                });
                if self.tool_log.len() > MAX_TOOL_ENTRIES {
                    self.tool_log.pop_front();
                }
            }

            TuiEvent::ToolCompleted {
                tool_use_id,
                duration_ms,
                success,
                error,
                ..
            } => {
                if let Some(ref at) = self.active_tool {
                    if at.tool_use_id == tool_use_id {
                        self.active_tool = None;
                    }
                }
                // Extract tool name before mutable borrow to satisfy borrow checker
                let tool_name_for_err = self
                    .tool_log
                    .iter()
                    .rev()
                    .find(|e| e.tool_use_id == tool_use_id)
                    .map(|e| e.name.clone());

                if let Some(entry) = self
                    .tool_log
                    .iter_mut()
                    .rev()
                    .find(|e| e.tool_use_id == tool_use_id)
                {
                    entry.state = if success {
                        ToolEntryState::Done
                    } else {
                        ToolEntryState::Failed
                    };
                    entry.duration_ms = Some(duration_ms);
                }

                if !success {
                    if let Some(err) = error {
                        let name = tool_name_for_err.unwrap_or_default();
                        {
                            self.push_output(OutputLine {
                                kind: OutputKind::Error,
                                text: format!("[{name}] Error: {err}"),
                                agent: None,
                                timestamp: Instant::now(),
                            });
                        }
                    }
                }
            }

            TuiEvent::ToolPendingApproval {
                agent_run_id,
                tool_name,
                tool_use_id,
                input,
            } => {
                let preview = input_preview(&input, 200);
                self.pending_approval = Some(PendingApprovalState {
                    agent_run_id,
                    tool_name: tool_name.clone(),
                    tool_use_id,
                    input,
                    input_preview: preview,
                    arrived_at: Instant::now(),
                });
                self.focused_panel = FocusedPanel::Approval;
                self.approval_focused_approve = true;
                self.push_output(OutputLine {
                    kind: OutputKind::System,
                    text: format!("⚠ Approval required for tool: {tool_name}"),
                    agent: None,
                    timestamp: Instant::now(),
                });
            }

            TuiEvent::CommitCreated {
                agent_name,
                short_sha,
                message,
                files_changed,
            } => {
                self.push_output(OutputLine {
                    kind: OutputKind::Success,
                    text: format!("✓ Committed [{short_sha}]: {message} ({files_changed} files)"),
                    agent: Some(agent_name),
                    timestamp: Instant::now(),
                });
                self.git_branch = self.session.as_ref().and_then(|s| s.git_branch.clone());
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
                        self.push_output(OutputLine {
                            kind: OutputKind::Success,
                            text: format!("✓ Task complete: {summary}"),
                            agent: None,
                            timestamp: Instant::now(),
                        });
                    }
                    SessionStatus::Failed { error } => {
                        self.phase = WorkflowPhase::Error;
                        self.push_output(OutputLine {
                            kind: OutputKind::Error,
                            text: format!("✗ Task failed: {error}"),
                            agent: None,
                            timestamp: Instant::now(),
                        });
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
                self.final_summary = summary.clone();
                self.push_output(OutputLine {
                    kind: OutputKind::Success,
                    text: format!("✓ {summary}"),
                    agent: None,
                    timestamp: Instant::now(),
                });
            }

            TuiEvent::RuntimeError(msg) => {
                self.phase = WorkflowPhase::Error;
                self.error_message = Some(msg.clone());
                self.push_output(OutputLine {
                    kind: OutputKind::Error,
                    text: format!("✗ {msg}"),
                    agent: None,
                    timestamp: Instant::now(),
                });
            }

            TuiEvent::Mouse(_) => {}
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Clear error on any keypress
        self.error_message = None;

        // Approval dialog takes focus
        if self.focused_panel == FocusedPanel::Approval {
            match key.code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    self.approval_focused_approve = !self.approval_focused_approve;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // Approval handled externally via ApprovalGate
                    // We just signal to the app layer
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.approval_focused_approve = true;
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.approval_focused_approve = false;
                }
                KeyCode::Esc => {
                    self.approval_focused_approve = false;
                }
                _ => {}
            }
            return;
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
                self.should_quit = true;
            }
            // Scroll output
            KeyCode::Down | KeyCode::Char('j') => {
                if self.output_scroll > 0 {
                    self.output_scroll -= 1;
                }
                if self.output_scroll == 0 {
                    self.output_auto_scroll = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.output_scroll += 1;
                self.output_auto_scroll = false;
            }
            KeyCode::PageDown => {
                self.output_scroll = self.output_scroll.saturating_sub(10);
                if self.output_scroll == 0 {
                    self.output_auto_scroll = true;
                }
            }
            KeyCode::PageUp => {
                self.output_scroll += 10;
                self.output_auto_scroll = false;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.output_scroll = 0;
                self.output_auto_scroll = true;
            }
            // Panel focus
            KeyCode::Tab => {
                self.focused_panel = match self.focused_panel {
                    FocusedPanel::Conversation => FocusedPanel::ToolLog,
                    FocusedPanel::ToolLog => FocusedPanel::Conversation,
                    FocusedPanel::Approval => FocusedPanel::Conversation,
                };
            }
            // Toggle debug log
            KeyCode::Char('l') | KeyCode::Char('L') => {
                self.show_log = !self.show_log;
            }
            _ => {}
        }
    }

    fn push_output(&mut self, line: OutputLine) {
        self.output_lines.push_back(line);
        if self.output_lines.len() > MAX_OUTPUT_LINES {
            self.output_lines.pop_front();
        }
        if self.output_auto_scroll {
            self.output_scroll = 0;
        }
    }

    fn log_info(&mut self, msg: String) {
        self.log.push_back(LogEntry {
            level: LogLevel::Info,
            message: msg,
            timestamp: Instant::now(),
        });
        if self.log.len() > MAX_LOG_ENTRIES {
            self.log.pop_front();
        }
    }

    /// Total tokens consumed.
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Elapsed time spinner character (based on tick).
    pub fn spinner_char(&self) -> char {
        const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        FRAMES[(self.tick as usize / 3) % FRAMES.len()]
    }

    /// Returns visible output lines for the given height, respecting scroll offset.
    pub fn visible_output(&self, height: usize) -> Vec<&OutputLine> {
        let n = self.output_lines.len();
        let end = n.saturating_sub(self.output_scroll);
        let start = end.saturating_sub(height);
        self.output_lines.range(start..end).collect()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn infer_phase_from_agent(name: &str) -> WorkflowPhase {
    match name {
        "coder" => WorkflowPhase::Coding,
        "qa" => WorkflowPhase::QaReview,
        "fixer" => WorkflowPhase::Fixing,
        _ => WorkflowPhase::Coding,
    }
}

fn input_preview(input: &serde_json::Value, max_len: usize) -> String {
    let s = match input {
        serde_json::Value::Object(map) => {
            // Show first key=value pair
            if let Some((k, v)) = map.iter().next() {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}={val}")
            } else {
                "{}".into()
            }
        }
        other => other.to_string(),
    };
    if s.len() > max_len {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn make_state() -> AppState {
        AppState::new("test task")
    }

    #[test]
    fn initial_state() {
        let s = make_state();
        assert_eq!(s.task, "test task");
        assert_eq!(s.phase, WorkflowPhase::Planning);
        assert!(!s.should_quit);
        assert!(s.output_lines.is_empty());
        assert_eq!(s.total_tokens(), 0);
    }

    #[test]
    fn tick_increments() {
        let mut s = make_state();
        s.handle_event(TuiEvent::Tick);
        assert_eq!(s.tick, 1);
        s.handle_event(TuiEvent::Tick);
        assert_eq!(s.tick, 2);
    }

    #[test]
    fn agent_output_pushed_to_lines() {
        let mut s = make_state();
        s.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "Hello output".into(),
        });
        assert_eq!(s.output_lines.len(), 1);
        assert_eq!(s.output_lines[0].text, "Hello output");
        assert_eq!(s.output_lines[0].kind, OutputKind::AgentText);
    }

    #[test]
    fn tool_started_creates_entry() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "read_file".into(),
            tool_use_id: "tid-1".into(),
            input: serde_json::json!({"path": "test.py"}),
            started_at: Instant::now(),
        });
        assert_eq!(s.tool_log.len(), 1);
        assert_eq!(s.tool_log[0].name, "read_file");
        assert_eq!(s.tool_log[0].state, ToolEntryState::Running);
        assert!(s.active_tool.is_some());
    }

    #[test]
    fn tool_completed_updates_entry() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "write_file".into(),
            tool_use_id: "tid-2".into(),
            input: serde_json::json!({}),
            started_at: Instant::now(),
        });
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "write_file".into(),
            tool_use_id: "tid-2".into(),
            duration_ms: 50.0,
            success: true,
            error: None,
        });
        assert_eq!(s.tool_log[0].state, ToolEntryState::Done);
        assert!(s.active_tool.is_none());
    }

    #[test]
    fn pending_approval_shown() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolPendingApproval {
            agent_run_id: "run-1".into(),
            tool_name: "bash_exec".into(),
            tool_use_id: "tid-3".into(),
            input: serde_json::json!({"command": "ls"}),
        });
        assert!(s.pending_approval.is_some());
        assert_eq!(s.focused_panel, FocusedPanel::Approval);
        let ap = s.pending_approval.as_ref().unwrap();
        assert_eq!(ap.tool_name, "bash_exec");
    }

    #[test]
    fn runtime_error_sets_phase() {
        let mut s = make_state();
        s.handle_event(TuiEvent::RuntimeError("boom".into()));
        assert_eq!(s.phase, WorkflowPhase::Error);
        assert!(s.error_message.is_some());
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
    fn output_buffer_bounded() {
        let mut s = make_state();
        for i in 0..MAX_OUTPUT_LINES + 100 {
            s.handle_event(TuiEvent::AgentOutput {
                agent_name: "x".into(),
                content: format!("line {i}"),
            });
        }
        assert_eq!(s.output_lines.len(), MAX_OUTPUT_LINES);
    }

    #[test]
    fn visible_output_respects_height() {
        let mut s = make_state();
        for i in 0..20 {
            s.handle_event(TuiEvent::AgentOutput {
                agent_name: "x".into(),
                content: format!("line {i}"),
            });
        }
        let visible = s.visible_output(5);
        assert_eq!(visible.len(), 5);
        // Last visible should be "line 19"
        assert_eq!(visible.last().unwrap().text, "line 19");
    }

    #[test]
    fn spinner_chars_rotate() {
        let s = make_state();
        let chars: Vec<char> = (0..30)
            .map(|i| {
                let mut st = AppState::new("x");
                st.tick = i;
                st.spinner_char()
            })
            .collect();
        // All chars should be valid spinner frames
        for c in &chars {
            assert!(['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'].contains(c));
        }
    }

    #[test]
    fn input_preview_truncates() {
        let val = serde_json::json!({"path": "a".repeat(100)});
        let preview = input_preview(&val, 20);
        // "…" is 3 UTF-8 bytes; allow for that overhead
        assert!(
            preview.chars().count() <= 22,
            "preview too long: {}",
            preview
        );
    }
}
