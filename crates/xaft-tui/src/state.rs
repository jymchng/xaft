//! Application state — the single source of truth for the TUI.
//!
//! `AppState` is updated by `TuiEvent`s and read by the renderer.
//! All mutations happen in the main event loop (single-threaded); no locking needed.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use ratatui::layout::Rect as TuiRect;

use xaft_runtime::session::{AgentSession, SessionStatus};

use crate::approval::{ApprovalDecision, ApprovalQueue, AutoApproveConfig};
use crate::bridge::TuiEvent;
use crate::layout::{LayoutManager, LayoutSolution, NavDirection, PaneType, SplitDirection};
use crate::renderer::TokenStreamRenderer;
use crate::widgets::diff::DiffViewerState;

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
    /// Live streaming renderer for the current agent turn.
    ///
    /// Tokens pushed here are committed to `visible` once per frame and
    /// rendered with a blinking cursor until the turn ends.
    pub stream: TokenStreamRenderer,
    /// Per-agent cost breakdown: agent_name → cumulative cost_usd.
    pub agent_costs: HashMap<String, f64>,
    /// Per-agent token breakdown: agent_name → cumulative tokens.
    pub agent_tokens: HashMap<String, u64>,

    // ── Diff viewer ───────────────────────────────────────────────────────────
    /// Full diff viewer state — hunk navigation, mode, scroll.
    pub diff: DiffViewerState,
    /// Most recent commit SHA (short form).
    pub last_commit_sha: Option<String>,

    // ── Layout ────────────────────────────────────────────────────────────────
    /// Dynamic pane layout manager.
    pub layout_manager: LayoutManager,
    /// Last solved layout (updated on Tick; used for directional navigation).
    pub last_solution: LayoutSolution,
    /// Last known terminal size (cols, rows).
    pub terminal_size: (u16, u16),

    // ── Approval ──────────────────────────────────────────────────────────────
    /// Live approval queue + history.
    pub approval_queue: ApprovalQueue,
    /// Gate decisions ready for the app layer: (tool_use_id, approved).
    pub pending_gate_decisions: Vec<(String, bool)>,

    // ── Input bar ─────────────────────────────────────────────────────────────
    /// Text being typed in the InputBar.
    pub input_buffer: String,
    /// Channel for submitting user messages to the agent at runtime.
    /// `None` until wired by `TuiApp`.
    pub user_message_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedPanel {
    Conversation,
    ToolLog,
    /// Diff viewer pane.
    Diff,
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
            stream: TokenStreamRenderer::new(""),
            agent_costs: HashMap::new(),
            agent_tokens: HashMap::new(),

            diff: DiffViewerState::new(),
            last_commit_sha: None,
            layout_manager: LayoutManager::default_coding_layout(),
            last_solution: LayoutSolution::default(),
            terminal_size: (200, 50),

            approval_queue: ApprovalQueue::new(AutoApproveConfig::default_safe()),
            pending_gate_decisions: Vec::new(),

            input_buffer: String::new(),
            user_message_tx: None,

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
                // Commit any buffered streaming tokens to the visible text
                self.stream.frame_update();
                // Update last solution for directional navigation
                let (w, h) = self.terminal_size;
                let area = TuiRect::new(0, 0, w, h);
                self.last_solution = self.layout_manager.solve(area);
                // Dynamic layout adaptation (every 4th tick ≈ 4×16ms = ~64ms)
                if self.tick % 4 == 0 {
                    self.auto_adapt();
                }
            }

            TuiEvent::Key(key) => self.handle_key(key),

            TuiEvent::Resize(w, h) => {
                self.terminal_size = (w, h);
                // Clamp scroll so it doesn't exceed the new terminal height
                let max_scroll = self.output_lines.len().saturating_sub(h as usize / 2);
                self.output_scroll = self.output_scroll.min(max_scroll);
            }

            TuiEvent::LlmCallStarting { agent_name, .. } => {
                // Flush the previous agent's streamed content before switching
                if !self.stream.text().is_empty() {
                    let flushed = self.stream.text().to_string();
                    let prev_agent = self.stream.agent_name.clone();
                    self.push_output(OutputLine {
                        kind: OutputKind::AgentText,
                        text: flushed,
                        agent: if prev_agent.is_empty() {
                            None
                        } else {
                            Some(prev_agent)
                        },
                        timestamp: Instant::now(),
                    });
                    self.stream.reset();
                }
                self.stream.agent_name = agent_name.clone();
                self.stream.is_active = true;
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
                let tokens = (input_tokens + output_tokens) as u64;
                self.total_input_tokens += input_tokens as u64;
                self.total_output_tokens += output_tokens as u64;
                self.total_cost_usd += cost_usd;
                self.total_llm_calls += 1;
                // Per-agent tracking
                *self.agent_costs.entry(agent_name.clone()).or_insert(0.0) += cost_usd;
                *self.agent_tokens.entry(agent_name.clone()).or_insert(0) += tokens;
                self.log_info(format!(
                    "[{agent_name}] LLM call: {input_tokens}+{output_tokens} tokens"
                ));
            }

            TuiEvent::AgentOutput {
                agent_name,
                content,
            } => {
                self.current_agent = agent_name.clone();
                self.stream.agent_name = agent_name.clone();
                // Feed into the stream renderer only — history is written on flush
                // (LlmCallStarting or AgentRunComplete). Avoids duplicates.
                self.stream.push_token(&content);
            }

            TuiEvent::AgentRunComplete {
                agent_name,
                turns,
                total_cost_usd,
            } => {
                // Flush any remaining stream content into history
                let flushed = self.stream.text().to_string();
                if !flushed.is_empty() {
                    let agent = if self.stream.agent_name.is_empty() {
                        agent_name.clone()
                    } else {
                        self.stream.agent_name.clone()
                    };
                    self.push_output(OutputLine {
                        kind: OutputKind::AgentText,
                        text: flushed,
                        agent: Some(agent),
                        timestamp: Instant::now(),
                    });
                    self.stream.reset();
                }
                self.stream.is_active = false;
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
                        // Split multi-line error into separate output lines so
                        // newlines render correctly (ratatui Paragraph wraps by
                        // width, not by embedded \n).
                        let header = format!("[{name}] FAILED");
                        self.push_output(OutputLine {
                            kind: OutputKind::Error,
                            text: header,
                            agent: None,
                            timestamp: Instant::now(),
                        });
                        for line in err.lines() {
                            if !line.trim().is_empty() {
                                self.push_output(OutputLine {
                                    kind: OutputKind::Error,
                                    text: format!("  {line}"),
                                    agent: None,
                                    timestamp: Instant::now(),
                                });
                            }
                        }
                    }
                }
            }

            TuiEvent::ToolPendingApproval {
                agent_run_id,
                tool_name,
                tool_use_id,
                input,
                risk: _,
            } => {
                let tool_name_clone = tool_name.clone();
                let result =
                    self.approval_queue
                        .push(tool_use_id.clone(), agent_run_id, tool_name, input);
                match result {
                    Some(decision) => {
                        // Auto-approved — send gate decision immediately
                        self.pending_gate_decisions
                            .push((tool_use_id, decision.is_approved()));
                    }
                    None => {
                        // Needs manual gate
                        self.focused_panel = FocusedPanel::Approval;
                        self.push_output(OutputLine {
                            kind: OutputKind::System,
                            text: format!("⚠ Approval required: {tool_name_clone}"),
                            agent: None,
                            timestamp: Instant::now(),
                        });
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
                self.push_output(OutputLine {
                    kind: OutputKind::Success,
                    text: format!("✓ Committed [{short_sha}]: {message} ({files_changed} files)"),
                    agent: Some(agent_name),
                    timestamp: Instant::now(),
                });
                self.git_branch = self.session.as_ref().and_then(|s| s.git_branch.clone());
            }

            TuiEvent::FileEditsCommitted {
                files,
                lines_added,
                lines_removed,
                diffs,
            } => {
                self.diff.push_diffs(&diffs, lines_added, lines_removed);
                // Auto-show diff pane when edits arrive
                self.layout_manager
                    .set_type_visible(PaneType::DiffViewer, true);
                let summary = format!(
                    "Edited {} file(s): +{lines_added}/−{lines_removed} lines",
                    files.len()
                );
                self.push_output(OutputLine {
                    kind: OutputKind::System,
                    text: summary,
                    agent: None,
                    timestamp: Instant::now(),
                });
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

            TuiEvent::Mouse(mouse) => {
                use crossterm::event::MouseEventKind;
                let (tw, th) = self.terminal_size;
                match mouse.kind {
                    MouseEventKind::Down(_) => {
                        // Clone solution to avoid borrow conflict
                        let solution = self.last_solution.clone();
                        self.layout_manager
                            .begin_drag(mouse.column, mouse.row, &solution);
                    }
                    MouseEventKind::Drag(_) => {
                        if self.layout_manager.is_dragging() {
                            self.layout_manager
                                .update_drag(mouse.column, mouse.row, tw, th);
                        }
                    }
                    MouseEventKind::Up(_) => {
                        self.layout_manager.end_drag();
                    }
                    _ => {}
                }
            }
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Clear error on any keypress
        self.error_message = None;

        // InputBar captures all printable keys when focused
        if self.layout_manager.focused_type() == Some(PaneType::InputBar) {
            match key.code {
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input_buffer.push(c);
                    return;
                }
                KeyCode::Backspace => {
                    self.input_buffer.pop();
                    return;
                }
                KeyCode::Enter => {
                    let msg = self.input_buffer.trim().to_string();
                    if !msg.is_empty() {
                        // Show in conversation pane
                        self.push_output(OutputLine {
                            kind: OutputKind::System,
                            text: format!("> {msg}"),
                            agent: None,
                            timestamp: Instant::now(),
                        });
                        // Forward to agent if channel is wired
                        if let Some(ref tx) = self.user_message_tx {
                            let _ = tx.send(msg);
                        }
                        self.input_buffer.clear();
                        // Update task label if not yet running
                        if self.task.is_empty() {
                            self.task = self.input_buffer.clone();
                        }
                    }
                    return;
                }
                KeyCode::Esc => {
                    // Escape from input bar → focus conversation
                    self.layout_manager.focus_type(PaneType::Chat);
                    self.sync_focused_panel();
                    return;
                }
                _ => {}
            }
        }

        // Approval dialog takes focus
        if self.focused_panel == FocusedPanel::Approval {
            match key.code {
                // Approve focused
                KeyCode::Char('a') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(id) = self
                        .approval_queue
                        .resolve_focused(ApprovalDecision::Approved)
                    {
                        self.pending_gate_decisions.push((id, true));
                    }
                    if !self.approval_queue.has_pending() {
                        self.focused_panel = FocusedPanel::Conversation;
                    }
                }
                // Reject focused
                KeyCode::Char('r') | KeyCode::Char('n') | KeyCode::Char('N') => {
                    if let Some(id) = self
                        .approval_queue
                        .resolve_focused(ApprovalDecision::Rejected)
                    {
                        self.pending_gate_decisions.push((id, false));
                    }
                    if !self.approval_queue.has_pending() {
                        self.focused_panel = FocusedPanel::Conversation;
                    }
                }
                // Skip (keep in queue, move focus)
                KeyCode::Char('s') => {
                    self.approval_queue.focus_next();
                }
                // Approve all
                KeyCode::Char('A') => {
                    use crate::approval::RiskLevel;
                    let decisions = self.approval_queue.approve_all_up_to(RiskLevel::Critical);
                    for (id, d) in decisions {
                        self.pending_gate_decisions.push((id, d.is_approved()));
                    }
                    if !self.approval_queue.has_pending() {
                        self.focused_panel = FocusedPanel::Conversation;
                    }
                }
                // Reject all
                KeyCode::Char('R') => {
                    for id in self.approval_queue.reject_all() {
                        self.pending_gate_decisions.push((id, false));
                    }
                    self.focused_panel = FocusedPanel::Conversation;
                }
                // Toggle history view
                KeyCode::Char('h') => {
                    self.approval_queue.show_history = !self.approval_queue.show_history;
                }
                // Navigate list
                KeyCode::Char('j') | KeyCode::Down => self.approval_queue.focus_next(),
                KeyCode::Char('k') | KeyCode::Up => self.approval_queue.focus_prev(),
                // Undo last
                KeyCode::Char('u') => {
                    self.approval_queue.undo_last();
                }
                KeyCode::Esc => {
                    if self.approval_queue.show_history {
                        self.approval_queue.show_history = false;
                    } else if !self.approval_queue.has_pending() {
                        self.focused_panel = FocusedPanel::Conversation;
                    }
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
                let max = self.output_lines.len().saturating_sub(1);
                self.output_scroll = (self.output_scroll + 1).min(max);
                self.output_auto_scroll = false;
            }
            KeyCode::PageDown => {
                self.output_scroll = self.output_scroll.saturating_sub(10);
                if self.output_scroll == 0 {
                    self.output_auto_scroll = true;
                }
            }
            KeyCode::PageUp => {
                let max = self.output_lines.len().saturating_sub(1);
                self.output_scroll = (self.output_scroll + 10).min(max);
                self.output_auto_scroll = false;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.output_scroll = 0;
                self.output_auto_scroll = true;
            }
            // Panel focus (Tab = next, Shift+Tab = prev)
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.layout_manager.focus_prev();
                } else {
                    self.layout_manager.focus_next();
                }
                self.sync_focused_panel();
            }

            // ── Diff viewer navigation (when Diff pane is focused) ─────────
            KeyCode::Char('n') if self.focused_panel == FocusedPanel::Diff => {
                self.diff.next_hunk();
            }
            KeyCode::Char('N') if self.focused_panel == FocusedPanel::Diff => {
                self.diff.prev_hunk();
            }
            KeyCode::Char('t') | KeyCode::Char('T') if self.focused_panel == FocusedPanel::Diff => {
                self.diff.toggle_mode();
            }
            KeyCode::Right if self.focused_panel == FocusedPanel::Diff => {
                self.diff.next_file();
            }
            KeyCode::Left if self.focused_panel == FocusedPanel::Diff => {
                self.diff.prev_file();
            }
            // j/k scroll in diff pane
            KeyCode::Down | KeyCode::Char('j') if self.focused_panel == FocusedPanel::Diff => {
                self.diff.scroll_down(1);
            }
            KeyCode::Up | KeyCode::Char('k') if self.focused_panel == FocusedPanel::Diff => {
                self.diff.scroll_up(1);
            }
            KeyCode::PageDown if self.focused_panel == FocusedPanel::Diff => {
                self.diff.scroll_down(10);
            }
            KeyCode::PageUp if self.focused_panel == FocusedPanel::Diff => {
                self.diff.scroll_up(10);
            }

            // ── Layout resize (Alt+H/J/K/L) ───────────────────────────────
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.layout_manager
                    .resize_focused(SplitDirection::Horizontal, -5);
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.layout_manager
                    .resize_focused(SplitDirection::Horizontal, 5);
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.layout_manager
                    .resize_focused(SplitDirection::Vertical, 5);
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.layout_manager
                    .resize_focused(SplitDirection::Vertical, -5);
            }

            // ── Directional pane navigation (Ctrl+H/J/K/L) ───────────────
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let sol = self.last_solution.clone();
                self.layout_manager
                    .navigate_directional(NavDirection::Left, &sol);
                self.sync_focused_panel();
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let sol = self.last_solution.clone();
                self.layout_manager
                    .navigate_directional(NavDirection::Right, &sol);
                self.sync_focused_panel();
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let sol = self.last_solution.clone();
                self.layout_manager
                    .navigate_directional(NavDirection::Down, &sol);
                self.sync_focused_panel();
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let sol = self.last_solution.clone();
                self.layout_manager
                    .navigate_directional(NavDirection::Up, &sol);
                self.sync_focused_panel();
            }

            // Toggle debug log
            KeyCode::Char('l') | KeyCode::Char('L') => {
                self.show_log = !self.show_log;
            }
            _ => {}
        }
    }

    /// Push a visual separator between tasks.
    pub fn push_separator(&mut self) {
        self.push_output(OutputLine {
            kind: OutputKind::System,
            text: "─".repeat(60),
            agent: None,
            timestamp: Instant::now(),
        });
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

    /// Dynamically adapt the layout based on current agent/diff/approval state.
    ///
    /// Called every ~64ms from the Tick handler. Only makes visible changes when
    /// the state actually warrants them to avoid constant layout churn.
    pub fn auto_adapt(&mut self) {
        // Show DiffViewer when there are diffs; hide when none
        let has_diffs = self.diff.has_diffs();
        if has_diffs && !self.layout_manager.is_type_visible(PaneType::DiffViewer) {
            self.layout_manager.auto_show(PaneType::DiffViewer);
        }

        // Show FileTree when there are file changes; hide after no changes
        if has_diffs && !self.layout_manager.is_type_visible(PaneType::FileTree) {
            self.layout_manager.auto_show(PaneType::FileTree);
        }

        // Approval panel: handled as an overlay (ApprovalWidget), not a layout pane.
        // Focus shift is already done in the ToolPendingApproval event handler.
    }

    /// Synchronise `focused_panel` from the layout manager's focused pane type.
    ///
    /// Called after any Tab / directional navigation so the old `FocusedPanel`
    /// enum stays consistent with the layout engine.
    pub fn sync_focused_panel(&mut self) {
        self.focused_panel = match self.layout_manager.focused_type() {
            Some(PaneType::Chat) | Some(PaneType::InputBar) => FocusedPanel::Conversation,
            Some(PaneType::AgentActivity) | Some(PaneType::LogConsole) => FocusedPanel::ToolLog,
            Some(PaneType::DiffViewer) => FocusedPanel::Diff,
            Some(PaneType::Approval) => FocusedPanel::Approval,
            // StatusBar, TokenDashboard, FileTree don't have a FocusedPanel variant;
            // keep Conversation as the catch-all.
            _ => FocusedPanel::Conversation,
        };
    }

    /// Total tokens consumed.
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Top agent by cost (for breakdown display).
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

    /// Summary of file edits for display: "N files +L/−R".
    pub fn edits_summary(&self) -> Option<String> {
        if !self.diff.has_diffs() {
            return None;
        }
        Some(self.diff.summary())
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
        // AgentOutput goes to the stream renderer; it's flushed to output_lines
        // on AgentRunComplete or LlmCallStarting (next agent turn).
        let mut s = make_state();
        s.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        s.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "Hello output".into(),
        });
        // Commit buffer to visible (normally done by Tick handler)
        s.stream.frame_update();
        // Still in stream, not yet in output_lines
        assert!(s.stream.text().contains("Hello output"));
        // Flush via AgentRunComplete
        s.handle_event(TuiEvent::AgentRunComplete {
            agent_name: "coder".into(),
            turns: 1,
            total_cost_usd: 0.0,
        });
        assert!(
            !s.output_lines.is_empty(),
            "should flush to output_lines on run complete"
        );
        let all_text: String = s.output_lines.iter().map(|l| l.text.as_str()).collect();
        assert!(all_text.contains("Hello output"));
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
        use crate::approval::RiskLevel;
        let mut s = make_state();
        // bash_exec with "rm -rf /" is High risk → should gate
        s.handle_event(TuiEvent::ToolPendingApproval {
            agent_run_id: "run-1".into(),
            tool_name: "bash_exec".into(),
            tool_use_id: "tid-3".into(),
            input: serde_json::json!({"command": "rm -rf /tmp/test"}),
            risk: RiskLevel::High,
        });
        assert!(s.approval_queue.has_pending());
        assert_eq!(s.focused_panel, FocusedPanel::Approval);
        let ap = s.approval_queue.focused().unwrap();
        assert_eq!(ap.tool_name, "bash_exec");
    }

    #[test]
    fn low_risk_tool_auto_approved() {
        use crate::approval::RiskLevel;
        let mut s = make_state();
        // read_file is Low → auto-approved, no dialog
        s.handle_event(TuiEvent::ToolPendingApproval {
            agent_run_id: "run-2".into(),
            tool_name: "read_file".into(),
            tool_use_id: "tid-auto".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
            risk: RiskLevel::Low,
        });
        assert!(!s.approval_queue.has_pending());
        assert_ne!(s.focused_panel, FocusedPanel::Approval);
        assert_eq!(s.pending_gate_decisions.len(), 1);
        assert!(s.pending_gate_decisions[0].1); // approved = true
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
        // Push via LlmCallStarting flushing (each new call flushes previous stream)
        let mut s = make_state();
        s.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "x".into(),
            call_index: 0,
        });
        // Push content through the stream and flush once per batch via new LlmCallStarting
        for i in 0..MAX_OUTPUT_LINES + 100 {
            // Reset stream for each "call" so each flush = one output_lines entry
            s.stream.reset();
            s.stream.push_token(&format!("line {i}"));
            s.stream.frame_update();
            // Flush by simulating next-agent start
            s.handle_event(TuiEvent::LlmCallStarting {
                agent_name: "x".into(),
                call_index: i + 1,
            });
        }
        assert!(s.output_lines.len() <= MAX_OUTPUT_LINES);
    }

    #[test]
    fn visible_output_respects_height() {
        // Push lines directly via LlmCallStarting flushes so they land in output_lines
        let mut s = make_state();
        for i in 0..20 {
            s.stream.reset();
            s.stream.push_token(&format!("line {i}"));
            s.stream.frame_update();
            // Flush: simulate next turn start
            s.handle_event(TuiEvent::LlmCallStarting {
                agent_name: "x".into(),
                call_index: i + 1,
            });
        }
        let visible = s.visible_output(5);
        assert_eq!(visible.len(), 5);
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
