//! Application state — the single source of truth for the TUI.
//!
//! `AppState` is updated by `TuiEvent`s and read by the renderer.
//! All mutations happen in the main event loop (single-threaded); no locking needed.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use ratatui::layout::Rect as TuiRect;

use xaft_runtime::session::{AgentSession, SessionStatus};

use crate::agent_tracker::AgentTracker;
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

    // ── Inline diff viewer ────────────────────────────────────────────────────
    /// Full inputs for in-flight `edit_file` / `write_file` tool calls,
    /// keyed by `tool_use_id`.  Retrieved on `ToolCompleted` to generate the
    /// inline diff block shown in the conversation stream.
    pub pending_file_inputs: HashMap<String, serde_json::Value>,

    // ── Transient indicators ──────────────────────────────────────────────────
    /// Last few lines of the current agent's thinking / response text.
    /// Shown transiently at the bottom of the chat pane.
    /// Replaced when the next tool starts or a new agent turn begins.
    /// Never stored in `output_lines`.
    pub active_agent_thinking: Option<String>,
    /// Instant at which the current LLM call started (set on LlmCallStarting,
    /// cleared on AgentRunComplete).  Used for elapsed-time display in the
    /// thinking indicator.
    pub agent_start_time: Option<std::time::Instant>,
    /// Whether to render inline diff lines (+ / - lines) in the conversation
    /// pane.  Toggled with Ctrl+O.  Defaults to `true`.
    pub show_diff_inline: bool,
    /// Inner rendering width of the chat pane (columns minus padding), updated
    /// by the conversation widget on each frame.  Used for wrap-aware scroll
    /// boundary computation in `handle_key`.  Interior-mutable so the widget
    /// can write it through a `&AppState` borrow.
    pub last_chat_inner_width: std::cell::Cell<usize>,

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

    // ── Agent activity tracker ────────────────────────────────────────────────
    /// Per-agent status and tool-call history for the activity widget.
    pub agent_tracker: AgentTracker,
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
    ToolCall,
    ToolResult,
    System,
    Error,
    Success,
    /// Agent name/icon marker line (◈ planner, ◉ coder, …) — rendered in purple.
    AgentMarker,
    /// User-submitted message shown in the conversation stream — rendered in default terminal color.
    UserMessage,
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

            pending_file_inputs: HashMap::new(),
            active_agent_thinking: None,
            agent_start_time: None,
            show_diff_inline: true,
            last_chat_inner_width: std::cell::Cell::new(78),

            input_buffer: String::new(),
            user_message_tx: None,

            focused_panel: FocusedPanel::Conversation,

            log: VecDeque::new(),
            show_log: false,
            tick: 0,
            should_quit: false,
            error_message: None,
            git_branch: None,

            agent_tracker: AgentTracker::new(),
        }
    }

    /// Reset tracker and stream state for a new task (called from `app.rs`).
    pub fn reset_for_new_task(&mut self) {
        self.agent_tracker.reset();
        self.stream.is_active = false;
        self.stream.agent_name.clear();
        self.active_agent_thinking = None;
        self.agent_start_time = None;
        self.show_diff_inline = true;
        self.pending_file_inputs.clear();
        self.phase = WorkflowPhase::Idle;
        self.current_agent.clear();
        self.current_agent_turns = 0;
        self.output_scroll = 0;
        self.output_auto_scroll = true;
        // Reset per-task token / cost stats so the UsageBar shows
        // current-task usage, not an accumulation from prior tasks.
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cost_usd = 0.0;
        self.total_llm_calls = 0;
        self.agent_costs.clear();
        self.agent_tokens.clear();
    }

    /// Handle a `TuiEvent` — the single mutation point for all state changes.
    pub fn handle_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                // Commit any buffered streaming tokens to the visible text
                self.stream.frame_update();
                // If scroll is at bottom and auto-scroll was somehow disabled,
                // re-enable it so new content keeps the view pinned to bottom.
                if self.output_scroll == 0 && !self.output_auto_scroll {
                    self.output_auto_scroll = true;
                }
                // Update last solution for directional navigation
                let (w, h) = self.terminal_size;
                let area = TuiRect::new(0, 0, w, h);
                self.last_solution = self.layout_manager.solve(area);
                // Dynamic layout adaptation (every 4th tick ≈ 4×16ms = ~64ms)
                if self.tick % 4 == 0 {
                    self.auto_adapt();
                }
                // Elapsed + token display moved to InputBar (working_indicator).
                // active_agent_thinking is reserved for streamed text excerpts only.
            }

            TuiEvent::Key(key) => self.handle_key(key),

            TuiEvent::Resize(w, h) => {
                self.terminal_size = (w, h);
                // Eagerly update inner-width estimate from the new terminal width so
                // scroll boundary computations (Up/PageUp key handlers) are consistent
                // with the new size immediately, before the next render frame sets the
                // exact value via last_chat_inner_width.set().
                let approx_inner = (w as usize).saturating_sub(4).max(20);
                self.last_chat_inner_width.set(approx_inner);
                if self.output_auto_scroll {
                    self.output_scroll = 0;
                } else {
                    let max = self.total_visual_rows(approx_inner).saturating_sub(1);
                    self.output_scroll = self.output_scroll.min(max);
                }
            }

            TuiEvent::LlmCallStarting { agent_name, .. } => {
                let agent_changed = self.current_agent != agent_name;
                self.stream.agent_name = agent_name.clone();
                self.stream.is_active = true;
                self.current_agent = agent_name.clone();
                self.phase = infer_phase_from_agent(&agent_name);
                self.log_info(format!("[{agent_name}] thinking…"));
                // Clear previous thinking — new turn is starting.
                self.active_agent_thinking = None;
                // Section 3: record start time for elapsed-time display.
                self.agent_start_time = Some(Instant::now());
                self.output_auto_scroll = true;
                // Only emit a permanent "agent started" line when the ACTIVE AGENT
                // CHANGES (not on every LLM turn). This prevents spamming the output
                // on every multi-turn response.
                if agent_changed {
                    let icon = match agent_name.as_str() {
                        "planner" | "summary" => "◈",
                        "coder" => "◉",
                        "qa" => "◎",
                        "fixer" => "◌",
                        _ => "◆",
                    };
                    self.push_output(OutputLine {
                        kind: OutputKind::AgentMarker,
                        text: format!("{icon} {agent_name}"),
                        agent: None,
                        timestamp: Instant::now(),
                    });
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
                // Per-agent tracking
                *self.agent_costs.entry(agent_name.clone()).or_insert(0.0) += cost_usd;
                *self.agent_tokens.entry(agent_name.clone()).or_insert(0) += tokens;
                self.log_info(format!(
                    "[{agent_name}] LLM call: {input_tokens}+{output_tokens} tokens"
                ));
                // Update tracker
                self.agent_tracker.on_llm_complete(&agent_name, cost_usd);
            }

            TuiEvent::AgentOutput {
                agent_name,
                content,
            } => {
                self.current_agent = agent_name.clone();
                self.stream.agent_name = agent_name.clone();

                // Push EVERY non-empty line permanently to output_lines so the
                // user can read and scroll through the full agent response.
                let non_empty: Vec<&str> =
                    content.lines().filter(|l| !l.trim().is_empty()).collect();
                for line in &non_empty {
                    self.push_output(OutputLine {
                        kind: OutputKind::AgentText,
                        text: line.to_string(),
                        agent: Some(agent_name.clone()),
                        timestamp: Instant::now(),
                    });
                }

                // Show the last line transiently so the user sees
                // what the agent just said without scrolling.
                if !non_empty.is_empty() {
                    let last = non_empty.last().unwrap_or(&"");
                    let truncated: String = last.chars().take(100).collect();
                    self.active_agent_thinking = Some(format!("  ⋯  {truncated}"));
                }
            }

            TuiEvent::AgentRunComplete {
                agent_name,
                turns,
                total_cost_usd,
            } => {
                // Agent done — clear transient displays.
                self.active_agent_thinking = None;
                self.agent_start_time = None;
                self.stream.is_active = false;
                self.current_agent_turns += turns;
                self.total_cost_usd = total_cost_usd.max(self.total_cost_usd);
                self.log_info(format!("[{agent_name}] run complete ({turns} turns)"));
                // Inline status: show agent completion in conversation pane
                self.push_output(OutputLine {
                    kind: OutputKind::System,
                    text: format!("    done ({turns} turns)"),
                    agent: None,
                    timestamp: Instant::now(),
                });
                // Update tracker
                self.agent_tracker.on_run_complete(&agent_name);
            }

            TuiEvent::AgentCancelled { agent_name, reason } => {
                self.phase = WorkflowPhase::Error;
                self.push_output(OutputLine {
                    kind: OutputKind::Error,
                    text: format!("[{agent_name}] Cancelled: {reason}"),
                    agent: Some(agent_name.clone()),
                    timestamp: Instant::now(),
                });
                // Update tracker
                self.agent_tracker.on_cancelled(&agent_name);
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
                    name: tool_name.clone(),
                    tool_use_id: tool_use_id.clone(),
                    input_preview: preview.clone(),
                    state: ToolEntryState::Running,
                    started_at,
                    duration_ms: None,
                });
                if self.tool_log.len() > MAX_TOOL_ENTRIES {
                    self.tool_log.pop_front();
                }
                // Tool starts — clear thinking and push inline tool call line.
                // Format: `  ◆ ReadFile(src/main.py)` — PascalCase with args in parens.
                let call_str = format_tool_call_inline(&tool_name, &input, 60);
                self.active_agent_thinking = None;
                self.push_output(OutputLine {
                    kind: OutputKind::ToolCall,
                    text: format!("  {call_str}"),
                    agent: None,
                    timestamp: Instant::now(),
                });
                // Section 2: for read-only tools, show a transient "Reading…"
                // indicator while the tool is in-flight.
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
                    self.active_agent_thinking = Some(hint);
                }
                // Store full input for edit_file / write_file so we can
                // generate an inline diff block when the tool completes.
                if tool_name == "edit_file" || tool_name == "write_file" {
                    self.pending_file_inputs
                        .insert(tool_use_id.clone(), input.clone());
                }
                // Update tracker — attribute to current_agent
                let agent = self.current_agent.clone();
                self.agent_tracker
                    .on_tool_start(&agent, &tool_name, &tool_use_id, &preview);
            }

            TuiEvent::ToolCompleted {
                tool_use_id,
                duration_ms,
                success,
                error,
                ..
            } => {
                // Clear any transient reading/searching indicator set by ToolStarted.
                self.active_agent_thinking = None;
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

                // Extract any pending file input before mutable borrows on output_lines.
                let file_diff_input = if success {
                    self.pending_file_inputs.remove(&tool_use_id)
                } else {
                    self.pending_file_inputs.remove(&tool_use_id);
                    None
                };

                // Append duration to the matching inline ToolCall entry.
                if success {
                    let name = tool_name_for_err.clone().unwrap_or_default();
                    let dur_str = if duration_ms >= 1000.0 {
                        format!("{:.1}s", duration_ms / 1000.0)
                    } else {
                        format!("{:.0}ms", duration_ms)
                    };
                    if let Some(entry) = self
                        .output_lines
                        .iter_mut()
                        .rev()
                        .find(|l| l.kind == OutputKind::ToolCall && l.text.contains(&name))
                    {
                        entry.text.push_str(&format!("  [{dur_str}]"));
                    }
                    // Inline diff block for file edits (Claude Code style)
                    if let Some(file_input) = file_diff_input {
                        let tname = tool_name_for_err.clone().unwrap_or_default();
                        push_inline_file_diff(self, &tname, &file_input);
                    }
                }

                if !success {
                    if let Some(err) = error {
                        let name = tool_name_for_err.unwrap_or_default();
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
                // Update tracker
                let agent = self.current_agent.clone();
                self.agent_tracker
                    .on_tool_complete(&agent, &tool_use_id, success);
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
                        // Section 7: inline approval indicator in conversation stream.
                        self.push_output(OutputLine {
                            kind: OutputKind::System,
                            text: format!(
                                "  ⚠  {} — approval required  ([a]yes [r]no [s]skip)",
                                tool_name_clone
                            ),
                            agent: None,
                            timestamp: Instant::now(),
                        });
                        // Update tracker — agent is now blocked
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
                // The planner's AgentOutput already pushed every summary line to
                // output_lines permanently (split-by-line). Re-pushing here would
                // double the content and cause visible_output_wrapped to show only
                // the tail of the duplicated block.
                // Just clear transient indicators so the summary is fully visible.
                self.active_agent_thinking = None;
                // Clear agent activity so pane is clean while waiting for next task
                self.agent_tracker.reset();
            }

            TuiEvent::AgentHandoff {
                from_agent,
                to_agent,
                summary: _,
            } => {
                self.push_output(OutputLine {
                    kind: OutputKind::AgentMarker,
                    text: format!("  ↝ {from_agent} → {to_agent}"),
                    agent: None,
                    timestamp: Instant::now(),
                });
                self.log_info(format!("handoff: {from_agent} → {to_agent}"));
            }

            TuiEvent::StreamToken { agent_name, token } => {
                // Accumulate streaming tokens into active_agent_thinking so
                // the TUI shows text appearing character-by-character while
                // the LLM is still generating. The full response will be
                // committed to output_lines when AgentOutput fires.
                if self.current_agent != agent_name {
                    self.current_agent = agent_name.clone();
                }
                let current = self.active_agent_thinking.take().unwrap_or_default();
                // Strip the leading "  ⋯  " prefix if present, then rebuild it.
                let bare = current.strip_prefix("  ⋯  ").unwrap_or(&current);
                let updated = format!("{bare}{token}");
                // Keep last 200 chars to avoid unbounded growth.
                let display: String = if updated.chars().count() > 200 {
                    updated.chars().rev().take(200).collect::<String>().chars().rev().collect()
                } else {
                    updated
                };
                self.active_agent_thinking = Some(format!("  ⋯  {display}"));
                self.stream.is_active = true;
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
                            kind: OutputKind::UserMessage,
                            text: format!("❯ {msg}"),
                            agent: None,
                            timestamp: Instant::now(),
                        });
                        // Forward to agent if channel is wired
                        if let Some(ref tx) = self.user_message_tx {
                            let _ = tx.send(msg.clone());
                        }
                        self.task = msg.clone();
                        self.input_buffer.clear();
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
            // Scroll output — Down scrolls 1 line at a time toward latest.
            // Once scroll reaches 0 (bottom), auto-scroll re-engages.
            KeyCode::Down | KeyCode::Char('j') => {
                self.output_scroll = self.output_scroll.saturating_sub(1);
                if self.output_scroll == 0 {
                    self.output_auto_scroll = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let w = self.last_chat_inner_width.get().max(20);
                let max = self.total_visual_rows(w).saturating_sub(1);
                self.output_scroll = (self.output_scroll + 1).min(max);
                self.output_auto_scroll = false;
            }
            KeyCode::PageDown => {
                self.output_scroll = self.output_scroll.saturating_sub(15);
                if self.output_scroll == 0 {
                    self.output_auto_scroll = true;
                }
            }
            KeyCode::PageUp => {
                let w = self.last_chat_inner_width.get().max(20);
                let max = self.total_visual_rows(w).saturating_sub(1);
                self.output_scroll = (self.output_scroll + 15).min(max);
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

            // Section 5: toggle inline diff expansion (Ctrl+O)
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.show_diff_inline = !self.show_diff_inline;
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
        const FRAMES: &[char] = &[
            '⠄', '⠆', '⠇', '⠋', '⠙', '⠸', '⠰', '⠠',
            '⠠', '⠰', '⠸', '⠙', '⠋', '⠇', '⠆', '⠄',
        ];
        FRAMES[(self.tick as usize / 4) % FRAMES.len()]
    }

    /// Rotating icon char for the active-work indicator: `✢ ✣ ✤ ✥`.
    pub fn indicator_icon(&self) -> char {
        const ICONS: &[char] = &['✢', '✣', '✤', '✥'];
        ICONS[(self.tick as usize / 15) % ICONS.len()]
    }

    /// Phase-specific verb shown in the working indicator, rotating for Coding.
    pub fn phase_verb(&self) -> &'static str {
        match self.phase {
            WorkflowPhase::Planning => "Planning",
            WorkflowPhase::Coding => {
                "Coding"
            }
            WorkflowPhase::QaReview => "Reviewing",
            WorkflowPhase::Fixing => "Fixing",
            _ => "Working",
        }
    }

    /// Full working indicator string shown in the `InputBar` while a phase is active.
    ///
    /// Format: `✢ Synthesizing… (5m 34s · ↓ 11.4k tokens)`
    ///
    /// Elapsed time and `↓ output_tokens` are included when an LLM call is in
    /// progress.  The `↓` arrow denotes tokens received from the model (output);
    /// the `↑` arrow (not currently shown) would denote tokens sent (input).
    pub fn working_indicator(&self) -> String {
        let icon = self.indicator_icon();
        let verb = self.phase_verb();
        if let Some(start) = self.agent_start_time {
            let elapsed_str = format_elapsed(start.elapsed());
            let out_tok_str = format_tokens_compact(self.total_output_tokens);
            format!("{icon} {verb}… ({elapsed_str} · ↓ {out_tok_str} tokens)")
        } else {
            format!("{icon} {verb}…")
        }
    }

    /// Returns visible output lines for the given height, respecting scroll offset.
    /// Visual rows a single output line occupies when rendered at `width` columns.
    pub fn visual_row_count_for(text: &str, width: usize) -> usize {
        if width == 0 {
            return 1;
        }
        let w = unicode_width::UnicodeWidthStr::width(text);
        if w == 0 { 1 } else { w.div_ceil(width) }
    }

    /// Total visual rows occupied by all lines in `output_lines` at `width` columns.
    /// Used to compute scroll boundaries that feel correct regardless of wrapping.
    pub fn total_visual_rows(&self, width: usize) -> usize {
        self.output_lines
            .iter()
            .map(|l| Self::visual_row_count_for(&l.text, width).max(1))
            .sum()
    }

    /// Return the output lines visible in a pane of `height` visual rows when
    /// rendered at `width` columns, with the view scrolled up by `scroll_rows`
    /// visual rows from the bottom.
    ///
    /// `scroll_rows = 0` means "pinned to bottom" — identical to the old
    /// `visible_output_wrapped`.  Increasing `scroll_rows` slides the window
    /// upward one visual row at a time, correctly accounting for wrapped lines
    /// that occupy more than one terminal row.
    pub fn visible_output_scrolled(
        &self,
        height: usize,
        width: usize,
        scroll_rows: usize,
    ) -> Vec<&OutputLine> {
        if height == 0 || width == 0 {
            return vec![];
        }

        let mut rows_to_skip = scroll_rows;
        let mut rows_collected = 0usize;
        let mut result: Vec<&OutputLine> = Vec::new();

        for line in self.output_lines.iter().rev() {
            let rcount = Self::visual_row_count_for(&line.text, width).max(1);

            if rows_to_skip >= rcount {
                // This line is entirely below the visible window — skip it.
                rows_to_skip -= rcount;
                continue;
            } else if rows_to_skip > 0 {
                // Line straddles the skip/visible boundary; include it so the
                // pane top isn't blank (ratatui can't split a logical line).
                rows_to_skip = 0;
            }

            // Stop before overflowing the visible area (same as visible_output_wrapped).
            if rows_collected + rcount > height && !result.is_empty() {
                break;
            }
            result.push(line);
            rows_collected += rcount;
            if rows_collected >= height {
                break;
            }
        }

        result.reverse();
        result
    }

    pub fn visible_output(&self, height: usize) -> Vec<&OutputLine> {
        let n = self.output_lines.len();
        let end = n.saturating_sub(self.output_scroll);
        let start = end.saturating_sub(height);
        self.output_lines.range(start..end).collect()
    }

    /// Return output lines that fit in `height` visual rows when rendered in a
    /// pane of `width` columns, accounting for text wrapping.
    ///
    /// Works backwards from the newest line (respecting `output_scroll`) so the
    /// bottom of the pane always shows the most recent content.  Lines with
    /// `[agent] ` prefixes have their full rendered width counted.
    pub fn visible_output_wrapped(&self, height: usize, width: usize) -> Vec<&OutputLine> {
        if height == 0 || width == 0 {
            return vec![];
        }
        let n = self.output_lines.len();
        let end = n.saturating_sub(self.output_scroll);

        let mut result: Vec<&OutputLine> = Vec::new();
        let mut rows_used = 0usize;

        for line in self.output_lines.range(..end).rev() {
            // Compute rendered width: optional "[agent] " prefix + text
            let prefix_w = line
                .agent
                .as_deref()
                .map(|a| {
                    // "[agent] " is rendered as format!("[{agent}] ")
                    unicode_width::UnicodeWidthStr::width(a) + 3
                })
                .unwrap_or(0);
            let text_w = unicode_width::UnicodeWidthStr::width(line.text.as_str());
            let total_w = prefix_w + text_w;
            // Ceiling division: how many terminal rows this line occupies
            let row_count = if total_w == 0 {
                1
            } else {
                total_w.div_ceil(width)
            };

            // If adding this line would overflow and we already have content, stop.
            if rows_used + row_count > height && !result.is_empty() {
                break;
            }
            rows_used += row_count;
            result.push(line);

            if rows_used >= height {
                break;
            }
        }

        result.reverse(); // oldest first → renders top-to-bottom
        result
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn infer_phase_from_agent(name: &str) -> WorkflowPhase {
    match name {
        "planner" | "summary" => WorkflowPhase::Planning,
        "coder" => WorkflowPhase::Coding,
        "qa" => WorkflowPhase::QaReview,
        "fixer" => WorkflowPhase::Fixing,
        _ => WorkflowPhase::Coding,
    }
}

/// Push an inline diff block to `output_lines` in Claude Code style.
///
/// For `edit_file`: computes a unified diff and renders it with line numbers,
/// context lines, and `+`/`-` markers.  Summary: `⎿  Added N lines, removed M lines`.
/// For `write_file`: shows `⎿  Added N lines`.
///
/// Diff lines use `OutputKind::Error` (red `-`), `OutputKind::Success` (green `+`),
/// and `OutputKind::ToolResult` (dim, for context lines and the summary).
///
/// Changed lines are capped at `MAX_CHANGED_LINES` to avoid flooding the pane.
fn push_inline_file_diff(
    state: &mut AppState,
    tool_name: &str,
    input: &serde_json::Value,
) {
    let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
    let ts = Instant::now();

    // Content width available for the text portion of each diff line.
    // Full prefix is 13 chars: 6 indent + 4 lineno + 1 space + 1 marker + 1 space.
    const PREFIX_WIDTH: usize = 13;
    let content_width = (state.terminal_size.0 as usize)
        .saturating_sub(PREFIX_WIDTH)
        .max(30);

    match tool_name {
        "edit_file" => {
            let old_content = input
                .get("old_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_content = input
                .get("new_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if old_content.is_empty() && new_content.is_empty() {
                return;
            }

            let diff = similar::TextDiff::from_lines(old_content, new_content);

            // Count total insertions / deletions for the summary line.
            let mut added = 0usize;
            let mut removed = 0usize;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Insert => added += 1,
                    similar::ChangeTag::Delete => removed += 1,
                    similar::ChangeTag::Equal => {}
                }
            }
            if added == 0 && removed == 0 {
                return;
            }

            // ⎿  Added N lines, removed M lines
            let summary = match (added, removed) {
                (a, 0) => format!(
                    "  ⎿  Added {} line{}",
                    a,
                    if a == 1 { "" } else { "s" }
                ),
                (0, r) => format!(
                    "  ⎿  Removed {} line{}",
                    r,
                    if r == 1 { "" } else { "s" }
                ),
                (a, r) => format!(
                    "  ⎿  Added {} line{}, removed {} line{}",
                    a,
                    if a == 1 { "" } else { "s" },
                    r,
                    if r == 1 { "" } else { "s" }
                ),
            };
            state.push_output(OutputLine {
                kind: OutputKind::ToolResult,
                text: summary,
                agent: None,
                timestamp: ts,
            });

            // Render unified diff with up to 3 context lines around each hunk.
            // Cap total changed lines shown to avoid flooding the pane.
            const CONTEXT_LINES: usize = 3;
            const MAX_CHANGED_LINES: usize = 30;
            let mut changed_shown = 0usize;

            'outer: for group in diff.grouped_ops(CONTEXT_LINES) {
                for op in &group {
                    for change in diff.iter_changes(op) {
                        let is_changed = change.tag() != similar::ChangeTag::Equal;
                        if is_changed {
                            if changed_shown >= MAX_CHANGED_LINES {
                                // Cap reached — push ellipsis and stop.
                                state.push_output(OutputLine {
                                    kind: OutputKind::System,
                                    text: format!(
                                        "      … {} more change{}",
                                        added + removed - changed_shown,
                                        if added + removed - changed_shown == 1 { "" } else { "s" }
                                    ),
                                    agent: None,
                                    timestamp: ts,
                                });
                                break 'outer;
                            }
                            changed_shown += 1;
                        }

                        let (lineno, marker, kind) = match change.tag() {
                            similar::ChangeTag::Delete => (
                                change.old_index().map(|i| i + 1),
                                '-',
                                OutputKind::Error,
                            ),
                            similar::ChangeTag::Insert => (
                                change.new_index().map(|i| i + 1),
                                '+',
                                OutputKind::Success,
                            ),
                            similar::ChangeTag::Equal => (
                                change.old_index().map(|i| i + 1),
                                ' ',
                                OutputKind::ToolResult,
                            ),
                        };

                        let lineno_str = lineno
                            .map(|n| format!("{n:>4}"))
                            .unwrap_or_else(|| "    ".to_string());
                        let content = change.value().trim_end_matches('\n');

                        // Split content into chunks that fit within content_width.
                        diff_line_chunks(content, content_width)
                            .iter()
                            .enumerate()
                            .for_each(|(i, chunk)| {
                                let text = if i == 0 {
                                    format!("      {lineno_str} {marker} {chunk}")
                                } else {
                                    // Continuation: align with content column (13 spaces total).
                                    format!("           {marker} {chunk}")
                                };
                                state.push_output(OutputLine {
                                    kind: kind.clone(),
                                    text,
                                    agent: None,
                                    timestamp: ts,
                                });
                            });
                    }
                }
            }
        }

        "write_file" => {
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let line_count = content.lines().count();
            if line_count == 0 {
                return;
            }
            state.push_output(OutputLine {
                kind: OutputKind::ToolResult,
                text: format!(
                    "  ⎿  Added {} line{}",
                    line_count,
                    if line_count == 1 { "" } else { "s" }
                ),
                agent: None,
                timestamp: ts,
            });
        }

        _ => {}
    }
}

/// Split `text` into chunks of at most `width` chars for diff line wrapping.
/// Returns at least one element (possibly empty string if text is empty).
fn diff_line_chunks(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return vec![text.to_string()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// Format a duration for the elapsed-time thinking indicator.
/// "42s", "1m 5s", etc.
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

fn input_preview(input: &serde_json::Value, max_len: usize) -> String {
    let s = match input {
        serde_json::Value::Object(map) => {
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

/// Convert `snake_case` tool name to `PascalCase`.
/// `list_files` → `ListFiles`, `bash_exec` → `BashExec`
fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

/// Format tool call as `ToolName(arg_value)` for the conversation stream.
/// Produces e.g. `ReadFile(src/main.py)`, `BashExec(pytest tests/)`.
fn format_tool_call_inline(tool_name: &str, input: &serde_json::Value, max_len: usize) -> String {
    let pascal = to_pascal_case(tool_name);
    // Extract the most meaningful argument value (first value in object, else raw)
    let arg = match input {
        serde_json::Value::Object(map) => {
            // Prefer non-key-like values: path, command, pattern, content snippet
            let preferred = ["path", "command", "pattern", "url", "query", "content"];
            let val = preferred
                .iter()
                .find_map(|k| map.get(*k))
                .or_else(|| map.values().next());
            match val {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            }
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let call = if arg.is_empty() {
        format!("◆ {pascal}()")
    } else {
        let budget = max_len.saturating_sub(pascal.len() + 4); // "◆ X()" overhead
        let truncated = if arg.len() > budget && budget > 2 {
            format!("{}…", &arg[..budget.saturating_sub(1)])
        } else {
            arg
        };
        format!("◆ {pascal}({truncated})")
    };
    call
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
    fn state_initial_phase_is_planning_for_non_empty_task() {
        let s = AppState::new("fix the bug");
        assert_eq!(s.phase, WorkflowPhase::Planning);
    }

    #[test]
    fn state_initial_phase_is_idle_for_empty_task() {
        let s = AppState::new("");
        assert_eq!(s.phase, WorkflowPhase::Idle);
        let s2 = AppState::new("   "); // whitespace-only
        assert_eq!(s2.phase, WorkflowPhase::Idle);
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
    fn agent_output_in_history_and_transient() {
        // AgentOutput goes to output_lines (permanent, scrollable) AND to
        // active_agent_thinking (last 2 lines, transient live indicator).
        let mut s = make_state();
        s.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "Hello thinking output\nSecond line here".into(),
        });
        // Must be in permanent output_lines
        let all_text: String = s.output_lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            all_text.contains("Hello thinking output"),
            "AgentOutput must be in output_lines (permanent history)"
        );
        // Must also appear in active_agent_thinking (last 2 lines)
        let thinking = s.active_agent_thinking.as_deref().unwrap_or("");
        assert!(
            !thinking.is_empty(),
            "AgentOutput must appear in active_agent_thinking"
        );
        // ToolStarted clears previous thinking and may set a transient
        // read indicator for read-only tools.
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "bash_exec".into(),
            tool_use_id: "t1".into(),
            input: serde_json::json!({"command": "echo hi"}),
            started_at: std::time::Instant::now(),
        });
        // bash_exec is not a read-only tool — thinking should be None.
        assert!(
            s.active_agent_thinking.is_none(),
            "ToolStarted for non-read tool must clear active_agent_thinking"
        );
        // output_lines still has the content (permanent)
        let all_text2: String = s.output_lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            all_text2.contains("Hello thinking output"),
            "history must survive ToolStarted"
        );
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
    fn output_buffer_unbounded() {
        // Verify output_lines has no cap — all pushed lines must be retained.
        let mut s = make_state();
        let n = MAX_OUTPUT_LINES + 100;
        for i in 0..n {
            s.handle_event(TuiEvent::AgentOutput {
                agent_name: "x".into(),
                content: format!("line {i}"),
            });
        }
        // Every AgentOutput line must be retained; no eviction should occur.
        assert_eq!(s.output_lines.len(), n, "output buffer must retain all lines (unbounded)");
    }

    #[test]
    fn visible_output_respects_height() {
        // Each agent switch pushes one permanent line; use 20 different agents.
        let mut s = make_state();
        for i in 0..20 {
            s.handle_event(TuiEvent::LlmCallStarting {
                agent_name: format!("agent_{i}"),
                call_index: i,
            });
        }
        let visible = s.visible_output(5);
        assert_eq!(visible.len(), 5);
    }

    #[test]
    fn spinner_chars_rotate() {
        let s = make_state();
        let chars: Vec<char> = (0..64)
            .map(|i| {
                let mut st = AppState::new("x");
                st.tick = i;
                st.spinner_char()
            })
            .collect();
        // All chars should be valid braille spinner frames (no blank ⠀)
        const VALID: &[char] = &[
            '⠄', '⠆', '⠇', '⠋', '⠙', '⠸', '⠰', '⠠',
            '⠠', '⠰', '⠸', '⠙', '⠋', '⠇', '⠆', '⠄',
        ];
        for c in &chars {
            assert!(VALID.contains(c), "unexpected spinner char: {:?}", c);
        }
        drop(s);
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

    // ── Inline diff viewer ────────────────────────────────────────────────────

    fn edit_file_event(tool_use_id: &str, path: &str, old: &str, new: &str) -> TuiEvent {
        TuiEvent::ToolCompleted {
            tool_name: "edit_file".into(),
            tool_use_id: tool_use_id.into(),
            duration_ms: 12.0,
            success: true,
            error: None,
        }
    }

    #[test]
    fn inline_diff_edit_file_pushes_red_and_green_lines() {
        let mut s = make_state();
        // Simulate ToolStarted storing the input
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-edit".into(),
            input: serde_json::json!({
                "path": "src/main.py",
                "old_content": "import random\n",
                "new_content": "import secrets\n"
            }),
            started_at: std::time::Instant::now(),
        });
        assert!(s.pending_file_inputs.contains_key("tid-edit"), "input stored");

        // Complete successfully
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-edit".into(),
            duration_ms: 8.0,
            success: true,
            error: None,
        });

        // pending input consumed
        assert!(!s.pending_file_inputs.contains_key("tid-edit"), "input removed");

        // Should have summary + diff lines in output_lines
        let has_summary = s.output_lines.iter().any(|l| {
            l.kind == OutputKind::ToolResult && l.text.contains("⎿")
        });
        let has_removed = s.output_lines.iter().any(|l| {
            l.kind == OutputKind::Error && l.text.contains("import random")
        });
        let has_added = s.output_lines.iter().any(|l| {
            l.kind == OutputKind::Success && l.text.contains("import secrets")
        });
        assert!(has_summary, "must have ⎿ summary line");
        assert!(has_removed, "must have red removed line");
        assert!(has_added, "must have green added line");
    }

    #[test]
    fn inline_diff_write_file_pushes_green_summary() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "write_file".into(),
            tool_use_id: "tid-write".into(),
            input: serde_json::json!({
                "path": "src/new.py",
                "content": "line1\nline2\nline3\n"
            }),
            started_at: std::time::Instant::now(),
        });
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "write_file".into(),
            tool_use_id: "tid-write".into(),
            duration_ms: 5.0,
            success: true,
            error: None,
        });

        let summary = s.output_lines.iter().find(|l| {
            l.kind == OutputKind::ToolResult && l.text.contains("⎿") && l.text.contains("3")
        });
        assert!(summary.is_some(), "must have ⎿ Added 3 lines summary");
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
            started_at: std::time::Instant::now(),
        });
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-fail".into(),
            duration_ms: 3.0,
            success: false,
            error: Some("pattern not found".into()),
        });

        // No diff lines on failure
        let has_diff = s.output_lines.iter().any(|l| {
            matches!(l.kind, OutputKind::Success | OutputKind::Error | OutputKind::ToolResult)
                && l.text.contains("⎿")
        });
        assert!(!has_diff, "must not show diff on failure");
        assert!(!s.pending_file_inputs.contains_key("tid-fail"), "input cleaned up");
    }

    #[test]
    fn inline_diff_caps_changed_lines() {
        let mut s = make_state();
        // Build a diff with many alternating changes so both + and - appear.
        // Even lines are shared (context), odd lines change — guarantees mixed output.
        let old: String = (0..40)
            .map(|i| if i % 2 == 0 { format!("shared {i}\n") } else { format!("old {i}\n") })
            .collect();
        let new: String = (0..40)
            .map(|i| if i % 2 == 0 { format!("shared {i}\n") } else { format!("new {i}\n") })
            .collect();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-big".into(),
            input: serde_json::json!({
                "path": "big.py",
                "old_content": old,
                "new_content": new
            }),
            started_at: std::time::Instant::now(),
        });
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "edit_file".into(),
            tool_use_id: "tid-big".into(),
            duration_ms: 50.0,
            success: true,
            error: None,
        });

        let red_lines = s.output_lines.iter()
            .filter(|l| l.kind == OutputKind::Error && l.text.starts_with("      "))
            .count();
        let green_lines = s.output_lines.iter()
            .filter(|l| l.kind == OutputKind::Success && l.text.starts_with("      "))
            .count();
        // Total changed lines capped at MAX_CHANGED_LINES=30
        assert!(red_lines + green_lines <= 30, "changed lines must be capped at 30");
        assert!(red_lines > 0, "must have some removed lines");
        assert!(green_lines > 0, "must have some added lines");
        // Overflow indicator present because 40 changed lines > 30 cap
        let has_overflow = s.output_lines.iter().any(|l| {
            l.kind == OutputKind::System && l.text.contains("more change")
        });
        assert!(has_overflow, "must show overflow indicator when cap hit");
    }

    #[test]
    fn to_pascal_case_converts_correctly() {
        assert_eq!(to_pascal_case("list_files"), "ListFiles");
        assert_eq!(to_pascal_case("edit_file"), "EditFile");
        assert_eq!(to_pascal_case("bash_exec"), "BashExec");
        assert_eq!(to_pascal_case("grep"), "Grep");
        assert_eq!(to_pascal_case("read_file"), "ReadFile");
    }

    // ── visible_output_scrolled ───────────────────────────────────────────────

    fn make_output_line(text: &str) -> OutputLine {
        OutputLine {
            kind: OutputKind::AgentText,
            text: text.to_string(),
            agent: None,
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn visible_output_scrolled_scroll0_same_as_wrapped() {
        // With scroll_rows=0 the function must behave like the old visible_output_wrapped.
        let mut s = make_state();
        // 5 single-row lines at width=80
        for i in 0..5 {
            s.push_output(make_output_line(&format!("line{i}")));
        }
        let via_scroll = s.visible_output_scrolled(3, 80, 0);
        let texts: Vec<&str> = via_scroll.iter().map(|l| l.text.as_str()).collect();
        // Bottom 3 lines: line2, line3, line4
        assert_eq!(texts, vec!["line2", "line3", "line4"]);
    }

    #[test]
    fn visible_output_scrolled_respects_scroll_offset() {
        let mut s = make_state();
        for i in 0..5 {
            s.push_output(make_output_line(&format!("line{i}")));
        }
        // scroll_rows=1 → skip newest 1 visual row (line4), show line1..line3
        let via_scroll = s.visible_output_scrolled(3, 80, 1);
        let texts: Vec<&str> = via_scroll.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn visible_output_scrolled_counts_wrapped_lines() {
        // A 20-char line at width=10 occupies 2 visual rows.
        let mut s = make_state();
        s.push_output(make_output_line("short")); // 1 visual row at w=10
        s.push_output(make_output_line("01234567890123456789")); // 20 chars → 2 rows
        s.push_output(make_output_line("end")); // 1 visual row

        // With scroll_rows=0, height=3: tries to fill 3 rows from bottom.
        // "end" = 1 row. "long" = 2 rows. 1+2=3 → stop.
        let vis = s.visible_output_scrolled(3, 10, 0);
        let texts: Vec<&str> = vis.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["01234567890123456789", "end"]);

        // scroll_rows=1 → skip 1 visual row from bottom (skip "end" 1 row).
        // Now: "long"(2 rows) at top of visible window. 2 < height=3 → add "short".
        let vis2 = s.visible_output_scrolled(3, 10, 1);
        let texts2: Vec<&str> = vis2.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts2, vec!["short", "01234567890123456789"]);
    }

    #[test]
    fn total_visual_rows_counts_wrapping() {
        let mut s = make_state();
        s.push_output(make_output_line("12345")); // 5 chars / 5 width = 1 row
        s.push_output(make_output_line("1234567890")); // 10 chars / 5 width = 2 rows
        s.push_output(make_output_line("")); // empty → 1 row
        assert_eq!(s.total_visual_rows(5), 4); // 1 + 2 + 1
    }

    #[test]
    fn scroll_max_uses_visual_rows() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        // 3 lines each 1 visual row at width=78 (default last_chat_inner_width)
        for i in 0..3 {
            s.push_output(make_output_line(&format!("line{i}")));
        }
        // total_visual_rows(78) = 3, max = 3 - 1 = 2
        for _ in 0..10 {
            s.handle_event(TuiEvent::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }));
        }
        // Should be capped at total_visual_rows - 1 = 2 (not logical line count)
        assert_eq!(s.output_scroll, 2, "scroll must be capped at total visual rows - 1");
    }

    // ── Section 1 — Scroll step = 1 ──────────────────────────────────────────

    #[test]
    fn scroll_step_is_one() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        // Push enough lines to scroll
        for i in 0..20 {
            s.push_output(OutputLine {
                kind: OutputKind::System,
                text: format!("line {i}"),
                agent: None,
                timestamp: Instant::now(),
            });
        }
        assert_eq!(s.output_scroll, 0);
        // Press Up once → scroll by 1
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert_eq!(s.output_scroll, 1, "Up key must scroll by 1");
        // Press Down once → back to 0
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert_eq!(s.output_scroll, 0, "Down key must scroll back by 1");
    }

    #[test]
    fn resize_preserves_scroll_when_not_auto_scroll() {
        let mut s = make_state();
        for i in 0..20 {
            s.push_output(OutputLine {
                kind: OutputKind::System,
                text: format!("line {i}"),
                agent: None,
                timestamp: Instant::now(),
            });
        }
        // Manually scroll up and disable auto-scroll
        s.output_scroll = 5;
        s.output_auto_scroll = false;
        // Resize should NOT snap back to 0
        s.handle_event(TuiEvent::Resize(100, 40));
        assert_eq!(s.output_scroll, 5, "resize must preserve scroll when auto_scroll=false");
    }

    #[test]
    fn resize_snaps_to_bottom_when_auto_scroll() {
        let mut s = make_state();
        for i in 0..20 {
            s.push_output(OutputLine {
                kind: OutputKind::System,
                text: format!("line {i}"),
                agent: None,
                timestamp: Instant::now(),
            });
        }
        s.output_scroll = 0;
        s.output_auto_scroll = true;
        s.handle_event(TuiEvent::Resize(100, 40));
        assert_eq!(s.output_scroll, 0, "resize must keep scroll=0 when auto_scroll=true");
    }

    #[test]
    fn resize_eagerly_updates_inner_width() {
        let mut s = make_state();
        // Initial default
        s.last_chat_inner_width.set(78);
        // Fire resize to new width
        s.handle_event(TuiEvent::Resize(40, 24));
        // Should immediately update estimate: 40 - 4 = 36
        assert_eq!(
            s.last_chat_inner_width.get(),
            36,
            "Resize must eagerly update inner-width estimate to (w - 4)"
        );
    }

    #[test]
    fn scroll_up_resize_down_reaches_bottom() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        // Push enough short lines (each 1 visual row at any reasonable width)
        for i in 0..50 {
            s.push_output(OutputLine {
                kind: OutputKind::System,
                text: format!("line {i:03}"),
                agent: None,
                timestamp: Instant::now(),
            });
        }
        // Simulate the widget setting inner_width (normally done on first render)
        s.last_chat_inner_width.set(76);

        // Scroll up 30 visual rows
        for _ in 0..30 {
            s.handle_event(TuiEvent::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }));
        }
        assert_eq!(s.output_scroll, 30, "should be scrolled up 30 rows");
        assert!(!s.output_auto_scroll, "auto-scroll must be disabled");

        // Simulate a terminal resize
        s.handle_event(TuiEvent::Resize(60, 30));

        // Press Down 30 times — must reach scroll=0 and re-engage auto-scroll
        for i in 0..30 {
            s.handle_event(TuiEvent::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }));
            if s.output_scroll == 0 {
                // Verify auto-scroll re-engaged on hitting bottom
                assert!(
                    s.output_auto_scroll,
                    "auto_scroll must be true when scroll reaches 0 (at press {i})"
                );
                break;
            }
        }
        assert_eq!(
            s.output_scroll, 0,
            "Down from scroll=30 must reach scroll=0 after at most 30 presses"
        );
        assert!(
            s.output_auto_scroll,
            "auto_scroll must be re-engaged at scroll=0"
        );
    }

    // ── Section 2 — Inline "Reading…" transient ──────────────────────────────

    #[test]
    fn reading_indicator_set_on_read_file() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "read_file".into(),
            tool_use_id: "t-read".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
            started_at: Instant::now(),
        });
        let thinking = s.active_agent_thinking.as_deref().unwrap_or("");
        assert!(
            thinking.contains("Reading"),
            "read_file must set Reading indicator, got: {:?}",
            thinking
        );
        assert!(
            thinking.contains("main.rs"),
            "indicator must contain filename, got: {:?}",
            thinking
        );
    }

    #[test]
    fn reading_indicator_set_on_list_files() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "list_files".into(),
            tool_use_id: "t-list".into(),
            input: serde_json::json!({}),
            started_at: Instant::now(),
        });
        let thinking = s.active_agent_thinking.as_deref().unwrap_or("");
        assert!(
            thinking.contains("Listing"),
            "list_files must set Listing indicator, got: {:?}",
            thinking
        );
    }

    #[test]
    fn reading_indicator_set_on_grep() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "grep".into(),
            tool_use_id: "t-grep".into(),
            input: serde_json::json!({"pattern": "fn main"}),
            started_at: Instant::now(),
        });
        let thinking = s.active_agent_thinking.as_deref().unwrap_or("");
        assert!(
            thinking.contains("fn main"),
            "grep must show search pattern, got: {:?}",
            thinking
        );
    }

    #[test]
    fn reading_indicator_cleared_on_tool_complete() {
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolStarted {
            tool_name: "read_file".into(),
            tool_use_id: "t-rc".into(),
            input: serde_json::json!({"path": "foo.rs"}),
            started_at: Instant::now(),
        });
        assert!(s.active_agent_thinking.is_some(), "indicator must be set while in-flight");
        s.handle_event(TuiEvent::ToolCompleted {
            tool_name: "read_file".into(),
            tool_use_id: "t-rc".into(),
            duration_ms: 10.0,
            success: true,
            error: None,
        });
        // ToolCompleted must clear the transient indicator immediately.
        assert!(
            s.active_agent_thinking.is_none(),
            "ToolCompleted must clear reading indicator, got: {:?}",
            s.active_agent_thinking
        );
    }

    // ── Section 4 — Context pct math ─────────────────────────────────────────

    #[test]
    fn context_pct_calc() {
        const CONTEXT_WINDOW_TOKENS: u64 = 262_112;
        // At 70% threshold
        let tok = CONTEXT_WINDOW_TOKENS * 70 / 100;
        let pct = (tok * 100) / CONTEXT_WINDOW_TOKENS;
        assert!(pct >= 69 && pct <= 70, "70% threshold calculation");
        // At 90%
        let tok90 = CONTEXT_WINDOW_TOKENS * 90 / 100;
        let pct90 = (tok90 * 100) / CONTEXT_WINDOW_TOKENS;
        assert!(pct90 >= 89 && pct90 <= 90, "90% threshold calculation");
    }

    // ── Section 5 — Ctrl+O toggles show_diff_inline ──────────────────────────

    #[test]
    fn ctrl_o_toggles_diff_inline() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        assert!(s.show_diff_inline, "defaults to true");
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert!(!s.show_diff_inline, "Ctrl+O must toggle to false");
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert!(s.show_diff_inline, "Ctrl+O must toggle back to true");
    }

    // ── Section 7 — Inline approval indicator ────────────────────────────────

    #[test]
    fn inline_approval_indicator_appears_in_stream() {
        use crate::approval::RiskLevel;
        let mut s = make_state();
        s.handle_event(TuiEvent::ToolPendingApproval {
            agent_run_id: "run-7".into(),
            tool_name: "bash_exec".into(),
            tool_use_id: "tid-7".into(),
            input: serde_json::json!({"command": "rm -rf /tmp/x"}),
            risk: RiskLevel::High,
        });
        // Must have inline indicator in output_lines
        let has_indicator = s.output_lines.iter().any(|l| {
            l.kind == OutputKind::System
                && l.text.contains('⚠')
                && l.text.contains("bash_exec")
        });
        assert!(has_indicator, "ToolPendingApproval must push inline ⚠ indicator to output_lines");
        // The approval dialog must still be active
        assert!(s.approval_queue.has_pending());
        assert_eq!(s.focused_panel, FocusedPanel::Approval);
    }

    // ── format_elapsed + format_tokens_compact ────────────────────────────────

    #[test]
    fn format_elapsed_seconds() {
        let d = std::time::Duration::from_secs(45);
        assert_eq!(format_elapsed(d), "45s");
    }

    #[test]
    fn format_elapsed_minutes() {
        let d = std::time::Duration::from_secs(125);
        assert_eq!(format_elapsed(d), "2m 5s");
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
