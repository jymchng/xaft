//! Application state — the single source of truth for the TUI.
//!
//! `AppState` is updated by `TuiEvent`s and produces `RenderMutation`s that
//! the main event loop applies to the `IncrementalRenderer`.
//! All mutations happen in the main event loop (single-threaded); no locking needed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::transport::ContentBlock;
use agtrs_workspace::WorkspaceStore;
use xaft_config::{MentionConfig, XaftConfig};
use xaft_runtime::UserMessage;
use xaft_runtime::session::{AgentSession, SessionStatus};

use crate::agent_tracker::AgentTracker;
use crate::approval::{ApprovalDecision, ApprovalQueue, AutoApproveConfig};
use crate::bridge::TuiEvent;
use crate::confirm::EscapeConfirmDialog;
use crate::input_bar::{InputAction, InputBar};
use crate::mention::ExpandedMessage;
use crate::prompt::build_prompt;
use crate::render::MarkdownRenderer;
use crate::slash::commands::build_registry_with_config;
use crate::slash::palette::SlashHistory;
use crate::slash::parser::SlashCommandParser;
use crate::slash::registry::SlashCommandRegistry;
use crate::slash::{
    AgentStatsMap, CommandContext, CommandResult, ConfigRow, ConfigSection, SlashCommand,
    SlashCommandExecuted,
};
use crate::transcript::{
    LineKind, RenderMutation, StyledLine, build_file_diff_lines, format_tool_call_inline,
    input_preview, to_pascal_case,
};
use crate::trigger::history::{HistoryTriggerHandler, InputHistoryStore};
use crate::trigger::mention::MentionTriggerHandler;
use crate::trigger::slash_command::SlashCommandTriggerHandler;
use crate::trigger::{ActiveTrigger, MatchKind, TriggerContext, TriggerRegistry};

// ── Background pipeline constants ─────────────────────────────────────────────

/// Maximum buffered `RenderMutation`s per background pipeline.
///
/// When a background pipeline exceeds this cap, additional mutations are
/// silently discarded and a truncation sentinel line is added once.
pub const MAX_BUFFERED_MUTATIONS: usize = 2000;

// ── Background pipeline types ─────────────────────────────────────────────────

/// Status of a detached background pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundStatus {
    Running,
    Completed,
    Failed,
}

/// A pipeline that has been detached to the background with `Ctrl+B`.
#[derive(Debug, Clone)]
pub struct BackgroundEntry {
    /// Monotonic pipeline ID assigned at detach time.
    pub id: u64,
    /// First ~60 chars of the user prompt.
    pub task_summary: String,
    /// Wall-clock instant when the task was submitted (not when detached).
    pub started_at: Instant,
    /// Accumulated `RenderMutation`s produced while detached.
    pub buffered: Vec<RenderMutation>,
    pub status: BackgroundStatus,
    /// True once the buffer has been capped and the sentinel appended.
    pub truncated: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Emit a signal on a bus, spawning a task only when a tokio runtime is active.
/// Falls back to blocking emit when called from a synchronous context (e.g. tests).
fn emit_signal_if_bus<E: Clone + Send + Sync + 'static>(bus: Option<Arc<SignalBus>>, signal: E) {
    if let Some(bus) = bus {
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                tokio::spawn(async move { bus.emit(signal).await });
            }
            Err(_) => {
                // No async runtime — build a single-threaded one for the emit.
                // This handles test contexts gracefully.
                let rt = tokio::runtime::Builder::new_current_thread().build().ok();
                if let Some(rt) = rt {
                    rt.block_on(async move { bus.emit(signal).await });
                }
            }
        }
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

/// Full state of the TUI application.
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
    pub input_bar: InputBar,
    /// Typed user-message channel. Replaces the old
    /// `UnboundedSender<String>` (F3 @-mention: structured `UserMessage`
    /// carries `Vec<ContentBlock>` with resolved `FileRef`s).
    pub user_message_tx: Option<tokio::sync::mpsc::UnboundedSender<UserMessage>>,

    // ── F3 @-mention state ───────────────────────────────────────────────────
    /// Workspace store used to resolve `@<path>` mentions at submit time.
    /// Set by `app.rs` when the TUI is bootstrapped (the working_dir is
    /// passed through from `RunRequest`).
    pub mention_workspace: Option<Arc<dyn WorkspaceStore>>,
    /// Root of the workspace (the working_dir). Used to resolve parent-
    /// traversal escape paths to absolute filesystem paths so they can be
    /// read via `tokio::fs` without going through the sandbox.
    pub mention_workspace_root: Option<std::path::PathBuf>,
    /// Mention configuration (caps, escape policy, allowlist).
    pub mention_config: MentionConfig,
    /// Cached list of workspace files for `@`-mention autocomplete.
    /// Loaded at startup via `FsWorkspaceStore::list()` in `app.rs`.
    pub mention_file_list: Vec<String>,
    /// The expanded message waiting to be sent. `Some` between
    /// submit-time resolution and either the user approving an escape
    /// dialog, the user cancelling, or the message being sent directly
    /// (no escape / session-wide approval).
    pub pending_expanded_message: Option<ExpandedMessage>,
    /// Original text the user typed, restored to the input bar on dialog
    /// cancel.
    pub pending_input_restore: Option<String>,
    /// Active escape confirmation dialog (if any).
    pub escape_dialog: Option<EscapeConfirmDialog>,
    /// `true` when the user pressed `A` to approve escape mentions for
    /// the rest of the session. Subsequent submissions skip the dialog.
    pub escape_approved_session: bool,
    /// Optional signal bus for emitting F3 signals (XaftMentionsResolved,
    /// XaftFileRefAttached, XaftEscapeMentionApproved, etc.).
    pub signal_bus: Option<Arc<SignalBus>>,

    // ── Agent activity tracker ────────────────────────────────────────────────
    pub agent_tracker: AgentTracker,

    // ── Timestamps ────────────────────────────────────────────────────────────
    pub agent_start_time: Option<Instant>,
    pub task_start_time: Option<Instant>,
    pub session_start_time: Option<Instant>,

    // ── Session / git ─────────────────────────────────────────────────────────
    pub last_commit_sha: Option<String>,
    pub git_branch: Option<String>,

    // ── Trigger system ────────────────────────────────────────────────────────
    /// Registry of all registered trigger handlers, built once in `new()`.
    pub trigger_registry: TriggerRegistry,
    /// Live state for the currently-open trigger dropdown, or `None` when no
    /// trigger is active. Replaces the former `mention_autocomplete` and
    /// `slash_palette` fields.
    pub active_trigger: Option<ActiveTrigger>,

    // ── Slash command system ──────────────────────────────────────────────────
    /// History of executed slash commands.
    pub slash_history: SlashHistory,
    /// Slash command registry (handlers keyed by SlashCommand discriminant).
    pub slash_registry: Arc<SlashCommandRegistry>,
    /// Interactive menu driver — at most one overlay active at a time.
    pub menu_driver: crate::menu::MenuDriver,
    /// Registry of slash commands that open interactive menu overlays.
    pub menu_registry: crate::menu::CommandMenuRegistry,
    /// Active mode manager — drives Shift+Tab cycling and /mode command.
    pub mode_manager: crate::mode::ModeManager,
    /// One-shot notification shown in mode footer for one render frame.
    pub mode_notification: Option<String>,
    /// Accumulated per-agent LLM stats (for /cost).
    pub agent_stats: Arc<RwLock<AgentStatsMap>>,
    /// Resolved application config (read-only, for slash handlers).
    pub xaft_config: Arc<XaftConfig>,
    /// Working directory for the current session.
    pub working_dir: PathBuf,

    // ── Markdown rendering ────────────────────────────────────────────────────
    /// Renders agent output Markdown to styled spans. Reconstructed on resize.
    pub markdown_renderer: MarkdownRenderer,

    // ── Background pipeline management ────────────────────────────────────────
    /// True while TUI events should be routed to the background buffer instead
    /// of the main transcript. Set by `Ctrl+B` (detach) and cleared when the
    /// background task completes or the user re-attaches.
    pub bg_mode: bool,
    /// Detached background pipelines, oldest first.
    pub background_entries: Vec<BackgroundEntry>,
    /// Monotonic counter used to assign unique IDs to pipelines.
    pub next_pipeline_id: u64,
    /// True after the user pressed `Ctrl+B` with multiple bg entries and we
    /// are waiting for a digit key to select which one to re-attach.
    pub awaiting_bg_select: bool,
    /// Set to true when the user starts a new task while a background pipeline
    /// is still running.  Prevents the bg `TaskComplete` from setting
    /// `task_done = true` and prematurely clearing the running-task indicator.
    pub bg_new_task_sent: bool,
    /// Set to true on the first quit attempt while bg pipelines are running.
    /// The second quit bypasses the warning.
    pub quit_confirmed_with_bg: bool,

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

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("phase", &self.phase)
            .field("task", &self.task)
            .field("input_bar", &self.input_bar)
            .field("has_workspace", &self.mention_workspace.is_some())
            .field("escape_approved_session", &self.escape_approved_session)
            .field("has_dialog", &self.escape_dialog.is_some())
            .field(
                "active_trigger",
                &self.active_trigger.as_ref().map(|at| at.scan.trigger_char),
            )
            .finish_non_exhaustive()
    }
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
    /// Construct an `AppState` for use in unit / integration tests.
    /// Skips the heavy initialisation done by `new` and leaves every
    /// F3 field at its default (no workspace, no dialog, no session
    /// approval). Tests can poke `escape_dialog` and
    /// `escape_approved_session` directly.
    pub fn new_for_test() -> Self {
        let s = AppState::new("test");
        s
    }

    pub fn new(task: impl Into<String>) -> Self {
        let task = task.into();
        let phase = if task.trim().is_empty() {
            WorkflowPhase::Idle
        } else {
            WorkflowPhase::Planning
        };

        let xaft_config = Arc::new(XaftConfig::default());
        let agent_stats = Arc::new(RwLock::new(AgentStatsMap::new()));
        let slash_registry = Arc::new(build_registry_with_config(
            Arc::clone(&xaft_config),
            Arc::clone(&agent_stats),
        ));

        // Build the trigger registry with all handlers.
        let mut trigger_registry = TriggerRegistry::new();
        trigger_registry.register(Arc::new(MentionTriggerHandler::new(
            MentionConfig::default(),
        )));
        trigger_registry.register(Arc::new(SlashCommandTriggerHandler::new(Arc::clone(
            &slash_registry,
        ))));
        let history_store = Arc::new(std::sync::RwLock::new(InputHistoryStore::new(200)));
        trigger_registry.register(Arc::new(HistoryTriggerHandler::new(history_store)));

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

            input_bar: InputBar::new(120),
            user_message_tx: None,

            mention_workspace: None,
            mention_workspace_root: None,
            mention_config: MentionConfig::default(),
            mention_file_list: Vec::new(),
            pending_expanded_message: None,
            pending_input_restore: None,
            escape_dialog: None,
            escape_approved_session: false,
            signal_bus: None,

            agent_tracker: AgentTracker::new(),

            agent_start_time: None,
            task_start_time: None,
            session_start_time: None,

            last_commit_sha: None,
            git_branch: None,

            trigger_registry,
            active_trigger: None,

            slash_history: SlashHistory::new(),
            slash_registry,
            menu_driver: crate::menu::MenuDriver::new(),
            menu_registry: crate::menu::CommandMenuRegistry::new(),
            mode_manager: crate::mode::ModeManager::default_builtin(),
            mode_notification: None,
            agent_stats,
            xaft_config,
            working_dir: PathBuf::from("."),

            markdown_renderer: MarkdownRenderer::new(120).with_indent(2),

            bg_mode: false,
            background_entries: Vec::new(),
            next_pipeline_id: 0,
            awaiting_bg_select: false,
            bg_new_task_sent: false,
            quit_confirmed_with_bg: false,

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

    /// True if the TUI is willing to accept a new task right now.
    ///
    /// Returns true when idle (no agent running), when the previous task is
    /// done, or when a pipeline has been moved to background (so the user can
    /// queue the next task without waiting).
    pub fn accepts_new_task(&self) -> bool {
        self.bg_mode || self.task_done || self.phase == WorkflowPhase::Idle
    }

    /// Create a new AppState with a specific signal bus (used in integration tests).
    pub fn new_with_bus(task: impl Into<String>, bus: Arc<SignalBus>) -> Self {
        let mut s = Self::new(task);
        s.signal_bus = Some(bus);
        s
    }

    /// Returns true if there's a pending run request (user_message_tx has a pending message).
    /// Used in tests to check that slash commands don't forward to the agent.
    pub fn has_pending_run_request(&self) -> bool {
        // The test uses a no-op channel; any send to user_message_tx is a run request.
        // We detect this by checking if the task changed from the initial state
        // or simply check task_start_time.
        self.task_start_time.is_some() && !self.task.is_empty() && self.phase != WorkflowPhase::Idle
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

    // ── F3 @-mention wiring ──────────────────────────────────────────────────

    /// Initialise F3 mention state. Called from `app.rs` once the runtime
    /// is bootstrapped and the working directory is known.
    pub fn init_mention(
        &mut self,
        workspace: Arc<dyn WorkspaceStore>,
        workspace_root: std::path::PathBuf,
        config: MentionConfig,
        signals: Option<Arc<SignalBus>>,
    ) {
        self.mention_workspace = Some(Arc::clone(&workspace));
        self.mention_workspace_root = Some(workspace_root.clone());
        self.mention_config = config.clone();
        self.signal_bus = signals;

        // Re-register the MentionTriggerHandler now that we have the workspace.
        let handler = Arc::new(MentionTriggerHandler::with_workspace(
            workspace,
            workspace_root,
            config,
        ));
        self.trigger_registry.replace('@', handler);
    }

    // ── Unified trigger system ───────────────────────────────────────────────

    /// Scan the input bar at the current cursor position. If the cursor is
    /// immediately after a registered trigger character with no whitespace
    /// between the char and the cursor, call the handler's `matches()` and
    /// populate `active_trigger`. Clears `active_trigger` otherwise.
    ///
    /// Replaces the former `refresh_autocomplete()` and `refresh_slash_palette()`.
    pub fn refresh_trigger(&mut self) {
        let cursor = self.input_bar.cursor();
        let lines = self.input_bar.lines();

        let line = match lines.get(cursor.row) {
            Some(l) => l.as_str(),
            None => {
                self.active_trigger = None;
                return;
            }
        };

        // Scan the current line for the nearest eligible trigger char.
        let Some(scan) = self.trigger_registry.active_for(line, cursor.col) else {
            self.active_trigger = None;
            return;
        };

        // Special case: '/' trigger only activates on a single-line buffer
        // whose first (and only) line starts with '/'.
        if scan.trigger_char == '/' && (lines.len() > 1 || !line.starts_with('/')) {
            self.active_trigger = None;
            return;
        }

        let handler = match self.trigger_registry.get(scan.trigger_char) {
            Some(h) => Arc::clone(h),
            None => {
                self.active_trigger = None;
                return;
            }
        };

        let ctx = TriggerContext {
            prefix: &scan.prefix,
            dir_prefix: &scan.dir_prefix,
            working_dir: &self.working_dir,
            terminal_cols: self.terminal_size.0,
        };

        let items = handler.matches(&ctx);

        if items.is_empty() && !handler.allows_free_text() {
            self.active_trigger = None;
            return;
        }

        // Preserve selected index when only the prefix changed (same trigger char —
        // avoids jumping on every keystroke).
        let selected = match &self.active_trigger {
            Some(prev) if prev.scan.trigger_char == scan.trigger_char => {
                prev.selected.min(items.len().saturating_sub(1))
            }
            _ => 0,
        };
        let scroll_top = match &self.active_trigger {
            Some(prev) if prev.scan.trigger_char == scan.trigger_char => prev.scroll_top,
            _ => 0,
        };

        self.active_trigger = Some(ActiveTrigger {
            scan,
            items,
            selected,
            scroll_top,
        });
    }

    /// Handle a keypress while a trigger dropdown is open.
    /// Returns `true` if the key was consumed by the trigger system.
    fn handle_trigger_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;

        let Some(ref at) = self.active_trigger else {
            return false;
        };
        let trigger_char = at.scan.trigger_char;
        let handler = match self.trigger_registry.get(trigger_char) {
            Some(h) => Arc::clone(h),
            None => return false,
        };
        let max_visible = handler.max_visible();

        match key.code {
            KeyCode::Up => {
                if let Some(ref mut at) = self.active_trigger {
                    at.select_prev(max_visible);
                }
                true
            }
            KeyCode::Down => {
                if let Some(ref mut at) = self.active_trigger {
                    at.select_next(max_visible);
                }
                true
            }
            KeyCode::Tab => {
                self.trigger_complete();
                true
            }
            KeyCode::Enter => {
                if handler.allows_free_text() {
                    // Submit the highlighted command (or fall through to submit
                    // raw text when the dropdown is empty).
                    if let Some(ref at) = self.active_trigger {
                        if let Some(item) = at.selected_item() {
                            let insert = item.insert.clone();
                            self.active_trigger = None;
                            // Treat the completed text as a submit.
                            self.input_bar.clear();
                            self.mutations
                                .push(RenderMutation::CommitLine(StyledLine::new(
                                    format!("❯ {insert}"),
                                    LineKind::UserMessage,
                                )));
                            self.handle_submit(insert);
                            return true;
                        }
                    }
                    // No candidate — dismiss and let the input bar submit.
                    self.active_trigger = None;
                    false
                } else {
                    // For non-free-text handlers (e.g. @-mention), Enter = Tab.
                    self.trigger_complete();
                    true
                }
            }
            KeyCode::Esc => {
                self.active_trigger = None;
                true
            }
            _ => false,
        }
    }

    /// Apply the currently-selected trigger item to the input buffer.
    fn trigger_complete(&mut self) {
        let (scan, item) = match &self.active_trigger {
            Some(at) => match at.selected_item() {
                Some(item) => (at.scan.clone(), item.clone()),
                None => return,
            },
            None => return,
        };
        let handler = match self.trigger_registry.get(scan.trigger_char) {
            Some(h) => Arc::clone(h),
            None => return,
        };

        let cursor = self.input_bar.cursor();
        let lines = self.input_bar.lines().to_vec();
        let line = match lines.get(cursor.row) {
            Some(l) => l.clone(),
            None => return,
        };
        let col = cursor.col.min(line.len());

        // Build the context so the handler can compute its insertion string.
        let ctx = TriggerContext {
            prefix: &scan.prefix,
            dir_prefix: &scan.dir_prefix,
            working_dir: &self.working_dir,
            terminal_cols: self.terminal_size.0,
        };
        let insertion = handler.on_select(&item, &ctx);

        // Replace from trigger_byte_pos to cursor position with the insertion.
        let new_line = format!(
            "{}{}{}",
            &line[..scan.trigger_byte_pos],
            insertion,
            &line[col..]
        );
        let mut all_lines = lines;
        all_lines[cursor.row] = new_line;
        self.input_bar.set_text(&all_lines.join("\n"));
        let new_col = scan.trigger_byte_pos + insertion.len();
        self.input_bar.set_cursor(cursor.row, new_col);

        self.active_trigger = None;

        // For directory items, immediately re-scan so the next level appears.
        if item.kind == MatchKind::Directory {
            self.refresh_trigger();
        }
    }

    /// Returns `true` when the escape confirmation dialog is currently
    /// shown. Used by the renderer to overlay the dialog on top of the
    /// normal prompt.
    pub fn has_escape_dialog(&self) -> bool {
        self.escape_dialog.is_some()
    }

    /// Apply a confirmation dialog outcome. Either sends the pending
    /// message via the channel (on approve) or restores the input bar
    /// (on cancel). Emits the appropriate F3 signals on the shared bus
    /// via `tokio::spawn` (fire-and-forget — signals are observational
    /// and do not gate the UI flow).
    pub fn resolve_escape_dialog(&mut self, outcome: crate::confirm::EscapeConfirmOutcome) {
        use crate::confirm::EscapeConfirmOutcome;

        let Some(expanded) = self.pending_expanded_message.take() else {
            // Stale call; dialog state already cleared.
            self.escape_dialog = None;
            return;
        };
        let dialog = self
            .escape_dialog
            .take()
            .unwrap_or_else(|| EscapeConfirmDialog::new(expanded.escape_mentions.clone()));

        match outcome {
            EscapeConfirmOutcome::ApproveOnce => {
                if let Some(bus) = self.signal_bus.clone() {
                    let entries: Vec<xaft_agent::signals::EscapeSignalEntry> = dialog
                        .mentions
                        .iter()
                        .map(|m| crate::escape_signal_entry(m, &m.absolute_path))
                        .collect();
                    tokio::spawn(async move {
                        bus.emit(xaft_agent::signals::XaftEscapeMentionApproved {
                            tokens: entries,
                            session_wide: false,
                        })
                        .await;
                    });
                }
                self.send_expanded(expanded);
            }
            EscapeConfirmOutcome::ApproveAllSession => {
                self.escape_approved_session = true;
                if let Some(bus) = self.signal_bus.clone() {
                    let entries: Vec<xaft_agent::signals::EscapeSignalEntry> = dialog
                        .mentions
                        .iter()
                        .map(|m| crate::escape_signal_entry(m, &m.absolute_path))
                        .collect();
                    tokio::spawn(async move {
                        bus.emit(xaft_agent::signals::XaftEscapeMentionApproved {
                            tokens: entries,
                            session_wide: true,
                        })
                        .await;
                    });
                }
                self.send_expanded(expanded);
            }
            EscapeConfirmOutcome::Cancel => {
                if let Some(bus) = self.signal_bus.clone() {
                    let entries: Vec<xaft_agent::signals::EscapeSignalEntry> = dialog
                        .mentions
                        .iter()
                        .map(|m| crate::escape_signal_entry(m, &m.absolute_path))
                        .collect();
                    tokio::spawn(async move {
                        bus.emit(xaft_agent::signals::XaftEscapeMentionDenied {
                            tokens: entries,
                            reason: "cancel".to_string(),
                        })
                        .await;
                    });
                }
                if let Some(text) = self.pending_input_restore.take() {
                    self.input_bar.set_text(&text);
                    let chars = self.input_bar.text().chars().count();
                    self.input_bar.set_cursor(0, chars);
                    self.push_prompt_update();
                }
            }
        }
    }

    /// Send a resolved `ExpandedMessage` via the user-message channel.
    /// Called either from the submit handler (no escape) or from
    /// [`resolve_escape_dialog`] (after approval).
    fn send_expanded(&mut self, expanded: ExpandedMessage) {
        let um = expanded.into_user_message();
        self.task = um.as_text_lossy();
        self.task_start_time = Some(Instant::now());
        if let Some(ref tx) = self.user_message_tx {
            let _ = tx.send(um);
        }
        self.pending_input_restore = None;
        self.push_prompt_update();
    }

    /// F3 fast path (no workspace configured): send a plain text
    /// `UserMessage` and update the task label. Preserves the pre-F3
    /// behaviour exactly.
    fn send_text_and_reset(&mut self, msg: String) {
        let text = msg.clone();
        if let Some(ref tx) = self.user_message_tx {
            let _ = tx.send(UserMessage::Text(msg));
        }
        self.task = text;
        self.task_start_time = Some(Instant::now());
        self.push_prompt_update();
    }

    // ── Slash command handling ────────────────────────────────────────────────

    /// Build a CommandContext from the current AppState.
    fn build_command_context(&self, args: String, command: SlashCommand) -> CommandContext {
        CommandContext {
            args,
            command,
            signals: self
                .signal_bus
                .clone()
                .unwrap_or_else(|| Arc::new(SignalBus::new())),
            config: Arc::clone(&self.xaft_config),
            session_id: self.session.as_ref().map(|s| s.id.to_string()),
            working_dir: self.working_dir.clone(),
            terminal_cols: self.terminal_size.0,
            llm_stats: Arc::clone(&self.agent_stats),
            conversation_store: None,
            session_store: None,
        }
    }

    /// Push a separator line for a slash command.
    fn push_slash_separator(&mut self, raw: &str) {
        const TARGET_LEN: usize = 62;
        // Format: "  ╌╌ /clear ╌╌╌╌..."
        let prefix = format!("  ╌╌ {} ", raw);
        let remaining = TARGET_LEN.saturating_sub(prefix.chars().count());
        let sep = format!("{}{}", prefix, "╌".repeat(remaining));
        self.mutations
            .push(RenderMutation::CommitLine(StyledLine::new(
                sep,
                LineKind::Separator,
            )));
    }

    /// Apply a CommandResult to the mutation list.
    pub fn apply_command_result(&mut self, result: CommandResult) {
        match result {
            CommandResult::Lines(lines) => {
                for line in lines {
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("  {}", line),
                            LineKind::System,
                        )));
                }
            }
            CommandResult::StyledLines(styled) => {
                for sl in styled {
                    self.mutations.push(RenderMutation::CommitLine(sl.clone()));
                }
            }
            CommandResult::Handled => {}
            CommandResult::Error(msg) => {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("    ✗ {}", msg),
                        LineKind::Error,
                    )));
            }
            CommandResult::OpenMenu(widget) => {
                self.menu_driver.open(widget);
                self.push_prompt_update();
            }
            CommandResult::ConfigDisplay(sections) => {
                let sep_width = 54usize;
                for section in sections {
                    // Section header: ── [core] ─────────────────
                    let pad = sep_width.saturating_sub(section.name.len() + 6);
                    let header = format!("  ── [{}] {}", section.name, "─".repeat(pad));
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            header,
                            LineKind::Separator,
                        )));
                    for row in &section.rows {
                        let marker = if row.is_overridden { "*" } else { " " };
                        let layer = row.source_layer.label();
                        let kind = match row.value_kind {
                            crate::slash::ConfigValueKind::Str => "str",
                            crate::slash::ConfigValueKind::Int => "int",
                            crate::slash::ConfigValueKind::Float => "f64",
                            crate::slash::ConfigValueKind::Bool => "bool",
                            crate::slash::ConfigValueKind::Array => "array",
                            crate::slash::ConfigValueKind::Table => "table",
                        };
                        let val: String = row.display_value.chars().take(22).collect();
                        let ellipsis = if row.display_value.chars().count() > 22 {
                            "…"
                        } else {
                            ""
                        };
                        let line = format!(
                            "  {marker} {key:<20} {val}{ellipsis:<1} {kind:<6} {layer}",
                            key = row.key,
                        );
                        let kind = if row.is_overridden {
                            LineKind::Success
                        } else {
                            LineKind::System
                        };
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(line, kind)));
                    }
                }
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        "  * = overridden from default  ·  /config set <key> <value> to change"
                            .to_string(),
                        LineKind::System,
                    )));
            }
        }
    }

    /// Execute a successfully parsed slash command.
    fn execute_slash_command(&mut self, cmd: SlashCommand, raw: &str) {
        let started = std::time::Instant::now();

        // Handle Quit specially.
        if matches!(cmd, SlashCommand::Quit) {
            self.push_slash_separator(raw);
            self.should_quit = true;
            self.slash_history.push(raw.to_string());
            // Emit signal.
            let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
            let signal = SlashCommandExecuted {
                name: "quit".to_string(),
                args: String::new(),
                success: true,
                duration_ms,
            };
            emit_signal_if_bus(self.signal_bus.clone(), signal);
            return;
        }

        let args = match &cmd {
            SlashCommand::Model { name } => name.clone(),
            SlashCommand::Resume { id } => id.clone().unwrap_or_default(),
            SlashCommand::Rewind { msg_index } => {
                msg_index.map(|n| n.to_string()).unwrap_or_default()
            }
            SlashCommand::Theme { name } => name.clone().unwrap_or_default(),
            SlashCommand::Pr { title } => title.clone().unwrap_or_default(),
            _ => {
                // Extract args from raw text.
                let after = raw.trim_start_matches('/');
                match after.find(char::is_whitespace) {
                    Some(pos) => after[pos + 1..].trim().to_string(),
                    None => String::new(),
                }
            }
        };

        let cmd_name = cmd.trigger_name().to_string();
        let handler = self.slash_registry.get(&cmd);
        self.push_slash_separator(raw);

        let ctx = self.build_command_context(args.clone(), cmd);
        let result = if handler.is_async() {
            // For now, execute async handlers with block_in_place.
            // TODO: show spinner while running.
            let fut = handler.execute_boxed_async(ctx);
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
                Err(_) => CommandResult::Error("No async runtime available.".into()),
            }
        } else {
            handler.execute(ctx)
        };

        let success = !matches!(result, CommandResult::Error(_));
        self.apply_command_result(result);

        // Record in history.
        self.slash_history.push(raw.to_string());

        // Emit SlashCommandExecuted signal.
        let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        let signal = SlashCommandExecuted {
            name: cmd_name,
            args,
            success,
            duration_ms,
        };
        emit_signal_if_bus(self.signal_bus.clone(), signal);
    }

    /// Handle the parse result from SlashCommandParser.
    fn handle_slash_parse_result(&mut self, result: Result<SlashCommand, String>, raw: &str) {
        match result {
            Err(msg) => {
                // Parse error (unknown command or bad args).
                self.push_slash_separator(raw);
                self.apply_command_result(CommandResult::Error(msg));
                // Emit failed signal.
                let name = {
                    let after = raw.trim_start_matches('/');
                    match after.find(char::is_whitespace) {
                        Some(pos) => after[..pos].to_ascii_lowercase(),
                        None => after.to_ascii_lowercase(),
                    }
                };
                let signal = SlashCommandExecuted {
                    name,
                    args: String::new(),
                    success: false,
                    duration_ms: 0.0,
                };
                emit_signal_if_bus(self.signal_bus.clone(), signal);
            }
            Ok(cmd) => {
                self.execute_slash_command(cmd, raw);
            }
        }
    }

    // refresh_slash_palette() replaced by refresh_trigger().

    /// Process a submit. If F3 mention resolution is configured
    /// (`mention_workspace` set), this blocks briefly to call
    /// `MentionResolver::expand` and either:
    /// - sends the resolved `UserMessage` immediately (no escape, or
    ///   session-wide approval);
    /// - shows the escape confirmation dialog (escape mentions + no
    ///   session-wide approval);
    /// - sends a `UserMessage::Text` with warnings inlined (file-not-
    ///   found, too large, etc.) if all mentions failed to resolve.
    ///
    /// If F3 is not configured, falls back to the pre-F3 path: send
    /// `UserMessage::Text(msg)` directly.
    fn handle_submit(&mut self, msg: String) {
        // Slash command interception — must happen BEFORE mention resolution.
        let trimmed = msg.trim();
        if trimmed.starts_with('/') && !trimmed.contains('\n') {
            match SlashCommandParser::parse(trimmed) {
                None => {
                    // Bare '/' — fall through to agent.
                }
                Some(result) => {
                    // It's a slash command (valid or invalid trigger).
                    self.active_trigger = None;
                    self.handle_slash_parse_result(result, trimmed);
                    // Re-render the prompt so the now-empty input bar is visible.
                    self.push_prompt_update();
                    return;
                }
            }
        }

        // F3 fast path: no workspace configured — preserve old behaviour.
        let Some(workspace) = self.mention_workspace.clone() else {
            self.send_text_and_reset(msg);
            return;
        };

        // F3 slow path: parse + resolve via the workspace store. The
        // workspace is small (in-memory in tests, on-disk in production
        // with bounded read), so a brief blocking call is acceptable.
        let config = self.mention_config.clone();
        let workspace_root = self.mention_workspace_root.clone();
        let parsed = crate::mention::MentionResolver::find_mentions(&msg);
        let (tx, rx) = std::sync::mpsc::channel();
        let msg_for_task = msg.clone();
        let workspace_for_task = workspace.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::try_current();
            let result = match rt {
                Ok(handle) => tokio::task::block_in_place(|| {
                    handle.block_on(crate::mention::MentionResolver::expand(
                        &msg_for_task,
                        workspace_for_task.as_ref(),
                        &config,
                        workspace_root.as_deref(),
                    ))
                }),
                Err(_) => {
                    // No tokio runtime; create one for the duration of the
                    // resolution. Fallback path used only in tests.
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("test runtime");
                    rt.block_on(crate::mention::MentionResolver::expand(
                        &msg_for_task,
                        workspace_for_task.as_ref(),
                        &config,
                        workspace_root.as_deref(),
                    ))
                }
            };
            let _ = tx.send(result);
        });
        let expanded = rx
            .recv()
            .expect("mention resolution thread must send a result");

        // Emit XaftMentionsResolved regardless of outcome.
        if let Some(bus) = self.signal_bus.clone() {
            let resolved_count = expanded
                .parts
                .iter()
                .filter(|b| matches!(b, ContentBlock::FileRef { .. }))
                .count();
            let entry = crate::mentions_resolved_entry(
                parsed.len(),
                resolved_count,
                expanded.warnings.len(),
                expanded.escape_mentions.len(),
                expanded
                    .parts
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::FileRef { byte_size, .. } => Some(*byte_size),
                        _ => None,
                    })
                    .sum(),
            );
            tokio::spawn(async move {
                bus.emit(entry).await;
            });
        }
        // Emit per-mention failure signals.
        if let Some(bus) = self.signal_bus.clone() {
            for w in &expanded.warnings {
                let entry =
                    crate::file_ref_not_found_entry(&crate::path_from_warning(w), &w.to_string());
                tokio::spawn({
                    let bus = bus.clone();
                    async move {
                        bus.emit(entry).await;
                    }
                });
            }
        }

        // Decide the next step based on policy + escape count.
        let has_escape = !expanded.escape_mentions.is_empty();
        if has_escape && !self.escape_approved_session {
            // Show the dialog. The user will resolve it later via
            // keys handled in `handle_escape_dialog_key` (called from
            // the main event loop when `state.escape_dialog.is_some()`).
            let dialog = EscapeConfirmDialog::new(expanded.escape_mentions.clone());
            // Render the dialog as a sequence of inline lines so the
            // user sees the contents above the input bar. The actual
            // state is stored in `self.escape_dialog` and resolved by
            // key events in `handle_escape_dialog_key`.
            self.mutations
                .push(RenderMutation::CommitLine(StyledLine::new(
                    format!("  ⚠  {}", dialog.header()),
                    LineKind::Error,
                )));
            for line in dialog.lines() {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("  │ {}", line.render()),
                        LineKind::Error,
                    )));
            }
            self.mutations
                .push(RenderMutation::CommitLine(StyledLine::new(
                    "  │  a=approve  A=approve all  c=cancel  ?=help".to_string(),
                    LineKind::Error,
                )));
            self.pending_expanded_message = Some(expanded);
            self.pending_input_restore = Some(msg);
            self.escape_dialog = Some(dialog);
            self.push_prompt_update();
        } else {
            // No escape, or session-wide approval already in effect:
            // send immediately.
            self.send_expanded(expanded);
        }
    }

    /// Handle a keypress while the escape dialog is active. Returns
    /// `true` if the key was consumed. The key handler in
    /// `handle_key()` should call this first (before the input bar).
    pub fn handle_escape_dialog_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crate::confirm::EscapeConfirmOutcome;
        use crossterm::event::KeyCode;

        if self.escape_dialog.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Char('a') | KeyCode::Enter => {
                self.resolve_escape_dialog(EscapeConfirmOutcome::ApproveOnce);
                true
            }
            KeyCode::Char('A') => {
                self.resolve_escape_dialog(EscapeConfirmOutcome::ApproveAllSession);
                true
            }
            KeyCode::Char('c') | KeyCode::Esc => {
                self.resolve_escape_dialog(EscapeConfirmOutcome::Cancel);
                true
            }
            KeyCode::Char('?') => {
                if let Some(d) = self.escape_dialog.as_mut() {
                    d.toggle_help();
                }
                self.push_prompt_update();
                true
            }
            _ => false,
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    // ── Background pipeline helpers ───────────────────────────────────────────

    /// Route a pipeline mutation: if in background mode, buffer it in the
    /// running background entry; otherwise push to `self.mutations`.
    fn push_pipeline_mutation(&mut self, mutation: RenderMutation) {
        if self.bg_mode {
            if let Some(entry) = self
                .background_entries
                .iter_mut()
                .rev()
                .find(|e| e.status == BackgroundStatus::Running)
            {
                if entry.buffered.len() < MAX_BUFFERED_MUTATIONS {
                    entry.buffered.push(mutation);
                } else if !entry.truncated {
                    entry.truncated = true;
                    entry
                        .buffered
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            "  [bg] earlier output truncated …".to_string(),
                            LineKind::System,
                        )));
                }
                return;
            }
            // No running bg entry found — bg_mode is stale; fall through to normal.
            self.bg_mode = false;
        }
        self.mutations.push(mutation);
    }

    /// Re-attach background entry at `idx`, replaying its buffered mutations.
    fn reattach_pipeline(&mut self, idx: usize) {
        if idx >= self.background_entries.len() {
            return;
        }
        let entry = self.background_entries.remove(idx);
        let buffered_count = entry.buffered.len();
        let is_running = entry.status == BackgroundStatus::Running;

        if is_running {
            // Resume foreground routing.
            self.bg_mode = false;
            self.phase = WorkflowPhase::Planning;
        }

        // Replay all buffered mutations.
        for mutation in entry.buffered {
            self.mutations.push(mutation);
        }

        let reattach_label = if is_running {
            format!(
                "  ↩ Resumed: {}  ·  Ctrl+B to background again",
                entry.task_summary
            )
        } else {
            let status = if entry.status == BackgroundStatus::Completed {
                "done"
            } else {
                "failed"
            };
            format!(
                "  ↩ Replayed output for: {}  ({})",
                entry.task_summary, status
            )
        };
        self.mutations
            .push(RenderMutation::CommitLine(StyledLine::new(
                reattach_label,
                LineKind::System,
            )));
        self.push_prompt_update();

        emit_signal_if_bus(
            self.signal_bus.clone(),
            xaft_agent::signals::XaftPipelineReattached {
                id: entry.id,
                buffered_mutations: buffered_count,
            },
        );
    }

    /// Handle `Ctrl+B`: detach current pipeline or re-attach a background one.
    fn handle_ctrl_b(&mut self) {
        // ── Re-attach path ─────────────────────────────────────────────────
        // Ctrl+B when idle (no active fg pipeline) opens the re-attach UI.
        if !self.phase.is_active() && !self.bg_mode {
            match self.background_entries.len() {
                0 => {
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            "  No background pipelines.".to_string(),
                            LineKind::System,
                        )));
                }
                1 => {
                    self.reattach_pipeline(0);
                }
                n => {
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("  Background pipelines — press 1–{n} to re-attach:"),
                            LineKind::System,
                        )));
                    for (i, entry) in self.background_entries.iter().enumerate() {
                        let status = match entry.status {
                            BackgroundStatus::Running => "running",
                            BackgroundStatus::Completed => "done",
                            BackgroundStatus::Failed => "failed",
                        };
                        let elapsed = format_elapsed(entry.started_at.elapsed());
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!(
                                    "  [{}]  {}  ·  {}  ({})",
                                    i + 1,
                                    &entry.task_summary,
                                    elapsed,
                                    status
                                ),
                                LineKind::System,
                            )));
                    }
                    self.awaiting_bg_select = true;
                }
            }
            return;
        }

        // ── Re-attach path when already in bg_mode ─────────────────────────
        // bg_mode=true means fg pipeline was detached but not yet completed.
        // Ctrl+B again re-attaches the running background entry.
        if self.bg_mode {
            // Find the most recently detached running entry.
            if let Some(idx) = self
                .background_entries
                .iter()
                .rposition(|e| e.status == BackgroundStatus::Running)
            {
                self.reattach_pipeline(idx);
            } else {
                // No running bg — clear stale flag.
                self.bg_mode = false;
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        "  No running background pipeline to re-attach.".to_string(),
                        LineKind::System,
                    )));
            }
            return;
        }

        // ── Detach path ────────────────────────────────────────────────────
        // Must have an active fg pipeline to detach.
        if !self.phase.is_active() {
            return;
        }

        let running_count = self
            .background_entries
            .iter()
            .filter(|e| e.status == BackgroundStatus::Running)
            .count();
        let max_bg = self.xaft_config.tui.max_background_tasks;
        if running_count >= max_bg {
            self.mutations
                .push(RenderMutation::CommitLine(StyledLine::new(
                    format!("  Max background tasks reached ({})", max_bg),
                    LineKind::System,
                )));
            return;
        }

        let id = self.next_pipeline_id;
        self.next_pipeline_id += 1;
        let task_summary: String = self.task.chars().take(60).collect();
        let started_at = self.task_start_time.unwrap_or_else(Instant::now);

        self.background_entries.push(BackgroundEntry {
            id,
            task_summary: task_summary.clone(),
            started_at,
            buffered: Vec::new(),
            status: BackgroundStatus::Running,
            truncated: false,
        });

        self.bg_mode = true;
        self.phase = WorkflowPhase::Idle;

        let new_running_count = self
            .background_entries
            .iter()
            .filter(|e| e.status == BackgroundStatus::Running)
            .count();
        self.mutations
            .push(RenderMutation::CommitLine(StyledLine::new(
                format!(
                    "  Agent moved to background  ·  Ctrl+B to re-attach  ·  {} running",
                    new_running_count
                ),
                LineKind::System,
            )));
        self.push_prompt_update();

        emit_signal_if_bus(
            self.signal_bus.clone(),
            xaft_agent::signals::XaftPipelineDetached { id, task_summary },
        );
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

            TuiEvent::Paste(s) => {
                self.error_message = None;
                self.input_bar.insert_str(&s);
                self.push_prompt_update();
            }

            TuiEvent::Resize(w, h) => {
                self.terminal_size = (w, h);
                self.input_bar.on_resize(w);
                self.markdown_renderer = MarkdownRenderer::new(w as usize).with_indent(2);
                self.mutations
                    .push(RenderMutation::Resize { cols: w, rows: h });
            }

            TuiEvent::LlmCallStarting { agent_name, .. } => {
                let agent_changed = self.current_agent != agent_name;
                self.current_agent = agent_name.clone();
                if !self.bg_mode {
                    self.phase = infer_phase_from_agent(&agent_name);
                }
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
                        self.push_pipeline_mutation(RenderMutation::FlushStream);
                        self.stream_active = false;
                    }
                    let display_name = capitalize_first(&agent_name);
                    self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
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
                self.push_pipeline_mutation(RenderMutation::StreamToken {
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
                    self.push_pipeline_mutation(RenderMutation::FlushStream);
                    self.stream_active = false;
                }
                // Render Markdown and commit each output line.
                let rendered = self.markdown_renderer.render(&content);
                for line in rendered {
                    self.push_pipeline_mutation(RenderMutation::CommitLine(
                        line.with_agent(&agent_name),
                    ));
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
                    self.push_pipeline_mutation(RenderMutation::FlushStream);
                }
                self.agent_tracker.on_run_complete(&agent_name);
            }

            TuiEvent::AgentCancelled { agent_name, reason } => {
                if !self.bg_mode {
                    self.phase = WorkflowPhase::Error;
                }
                self.stream_active = false;
                self.push_pipeline_mutation(RenderMutation::FlushStream);
                self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
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
                    self.push_pipeline_mutation(RenderMutation::FlushStream);
                    self.stream_active = false;
                }

                let call_str = format_tool_call_inline(&tool_name, &input, 60);
                self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                    format!("  {call_str}"),
                    LineKind::ToolCall,
                )));

                // Transient reading hint for read-only tools (foreground only).
                if !self.bg_mode
                    && matches!(tool_name.as_str(), "read_file" | "list_files" | "grep")
                {
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
                    self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                        format!("    ✓ {dur_str}"),
                        LineKind::ToolResult,
                    )));
                    if let Some(file_input) = file_diff_input {
                        let (cols, _) = self.terminal_size;
                        for line in build_file_diff_lines(&tool_name, &file_input, cols) {
                            self.push_pipeline_mutation(RenderMutation::CommitLine(line));
                        }
                    }
                } else if let Some(err) = error {
                    let mut err_lines = err.lines().filter(|l| !l.trim().is_empty());
                    let first = err_lines.next().unwrap_or("failed");
                    self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                        format!("    ✗ {first}"),
                        LineKind::Error,
                    )));
                    for line in err_lines {
                        self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                            format!("      {line}"),
                            LineKind::Error,
                        )));
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
                if self.bg_mode {
                    // Background pipeline is waiting for approval — alert the user
                    // without blocking the foreground.
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            "  [bg] Task paused — waiting for approval. Press Ctrl+B to re-attach."
                                .to_string(),
                            LineKind::System,
                        )));
                    // Still push the tool into the approval queue so it resolves
                    // normally once the pipeline is re-attached.
                    let _ = self.approval_queue.push(
                        tool_use_id.clone(),
                        agent_run_id,
                        tool_name,
                        input,
                    );
                } else {
                    let tool_name_clone = tool_name.clone();
                    let result = self.approval_queue.push(
                        tool_use_id.clone(),
                        agent_run_id,
                        tool_name,
                        input,
                    );
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
            }

            TuiEvent::CommitCreated {
                agent_name,
                short_sha,
                message,
                files_changed,
            } => {
                self.last_commit_sha = Some(short_sha.clone());
                self.git_branch = self.session.as_ref().and_then(|s| s.git_branch.clone());
                self.push_pipeline_mutation(RenderMutation::CommitLine(
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
                    self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
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
                        if !self.bg_mode {
                            self.phase = WorkflowPhase::Done;
                            self.task_done = true;
                            self.final_summary = summary.clone();
                        }
                        let s = summary.clone();
                        self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                            format!("✓ Task complete: {s}"),
                            LineKind::Success,
                        )));
                    }
                    SessionStatus::Failed { error } => {
                        if !self.bg_mode {
                            self.phase = WorkflowPhase::Error;
                        }
                        let e = error.clone();
                        self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                            format!("✗ Task failed: {e}"),
                            LineKind::Error,
                        )));
                    }
                    SessionStatus::Cancelled => {
                        if !self.bg_mode {
                            self.phase = WorkflowPhase::Error;
                        }
                    }
                    _ => {}
                }
                self.session = Some(session);
            }

            TuiEvent::TaskComplete { summary, session } => {
                if self.bg_mode {
                    // Background pipeline completed successfully.
                    self.bg_mode = false;
                    if let Some(entry) = self
                        .background_entries
                        .iter_mut()
                        .rev()
                        .find(|e| e.status == BackgroundStatus::Running)
                    {
                        let elapsed = format_elapsed(entry.started_at.elapsed());
                        let task_summary = entry.task_summary.clone();
                        let id = entry.id;
                        let duration_ms = entry.started_at.elapsed().as_secs_f64() * 1000.0;
                        entry.status = BackgroundStatus::Completed;
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!("[bg done]  {}  ·  {}", task_summary, elapsed),
                                LineKind::System,
                            )));
                        emit_signal_if_bus(
                            self.signal_bus.clone(),
                            xaft_agent::signals::XaftBackgroundPipelineComplete {
                                id,
                                task_summary,
                                success: true,
                                duration_ms,
                            },
                        );
                    }
                    // Always store the completed session for resume chaining.
                    self.session = Some(session);
                    if !self.bg_new_task_sent {
                        // No new task was queued — treat as normal task completion.
                        self.phase = WorkflowPhase::Done;
                        self.task_done = true;
                        self.final_summary = summary;
                        if self.stream_active {
                            self.mutations.push(RenderMutation::FlushStream);
                            self.stream_active = false;
                        }
                        self.mutations.push(RenderMutation::SetEphemeral(None));
                        self.agent_tracker.reset();
                    } else {
                        // A new task is already running — don't set task_done,
                        // so the app loop keeps agent_running=true for the new task.
                        self.bg_new_task_sent = false;
                        self.agent_tracker.reset();
                    }
                } else {
                    self.phase = WorkflowPhase::Done;
                    self.task_done = true;
                    self.final_summary = summary;
                    // Store the completed session so the NEXT task in this TUI
                    // session can pass resume_session_id and get full prior context.
                    self.session = Some(session);
                    // Flush any open stream.
                    if self.stream_active {
                        self.mutations.push(RenderMutation::FlushStream);
                        self.stream_active = false;
                    }
                    self.mutations.push(RenderMutation::SetEphemeral(None));
                    self.agent_tracker.reset();
                }
            }

            TuiEvent::BackgroundPipelineComplete { .. } => {
                // Generated by EventBridge from the XaftBackgroundPipelineComplete signal.
                // The actual bg-completion logic is handled in the TaskComplete arm above
                // (which fires first). This variant exists for external signal consumers;
                // ignore it in the AppState handler to avoid double-processing.
            }

            TuiEvent::AgentHandoff {
                from_agent,
                to_agent,
                ..
            } => {
                if self.stream_active {
                    self.push_pipeline_mutation(RenderMutation::FlushStream);
                    self.stream_active = false;
                }
                self.push_pipeline_mutation(RenderMutation::CommitLine(StyledLine::new(
                    format!("  ↝ {from_agent} → {to_agent}"),
                    LineKind::AgentMarker,
                )));
            }

            TuiEvent::RuntimeError(msg) => {
                self.error_message = Some(msg.clone());
                if self.bg_mode {
                    // Background pipeline failed.
                    self.bg_mode = false;
                    if let Some(entry) = self
                        .background_entries
                        .iter_mut()
                        .rev()
                        .find(|e| e.status == BackgroundStatus::Running)
                    {
                        let elapsed = format_elapsed(entry.started_at.elapsed());
                        let task_summary = entry.task_summary.clone();
                        let id = entry.id;
                        let duration_ms = entry.started_at.elapsed().as_secs_f64() * 1000.0;
                        entry.status = BackgroundStatus::Failed;
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!("[bg failed]  {}  ·  {}", task_summary, elapsed),
                                LineKind::Error,
                            )));
                        emit_signal_if_bus(
                            self.signal_bus.clone(),
                            xaft_agent::signals::XaftBackgroundPipelineComplete {
                                id,
                                task_summary,
                                success: false,
                                duration_ms,
                            },
                        );
                    }
                    if !self.bg_new_task_sent {
                        self.phase = WorkflowPhase::Error;
                        self.task_done = true;
                    } else {
                        self.bg_new_task_sent = false;
                    }
                } else {
                    self.phase = WorkflowPhase::Error;
                    self.mutations
                        .push(RenderMutation::CommitLine(StyledLine::new(
                            format!("✗ {msg}"),
                            LineKind::Error,
                        )));
                }
            }

            TuiEvent::Mouse(_) => {
                // Mouse events not used in conversational mode.
            }

            TuiEvent::MemoryStored {
                ref content_summary,
                ref tags,
                ref agent_name,
            } => {
                let tags_str = if tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", tags.join(", "))
                };
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("[MEMORY] {agent_name} remembered: {content_summary}{tags_str}"),
                        LineKind::System,
                    )));
            }

            TuiEvent::MemoryRecalled {
                ref query,
                results_count,
            } => {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("[MEMORY] Recalled {results_count} results for: {query}"),
                        LineKind::System,
                    )));
            }

            TuiEvent::ContextCompacted {
                ref agent_name,
                messages_removed,
                tokens_saved,
            } => {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!(
                            "  [compact]  {} message(s) summarised  ·  ~{} tokens freed  ({})",
                            messages_removed, tokens_saved, agent_name,
                        ),
                        LineKind::System,
                    )));
            }

            TuiEvent::SpecialistAgentStarted {
                ref agent_name,
                ref role,
                ref task_preview,
                ..
            } => {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("  ↳ [spawn] {agent_name} ({role}): {task_preview}"),
                        LineKind::AgentMarker,
                    )));
            }

            TuiEvent::SpecialistAgentCompleted {
                ref agent_name,
                success,
                duration_ms,
                ..
            } => {
                let marker = if success { "✓" } else { "✗" };
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!(
                            "  {marker} [spawn done] {agent_name}  ·  {:.1}s",
                            duration_ms / 1000.0
                        ),
                        if success {
                            LineKind::Success
                        } else {
                            LineKind::Error
                        },
                    )));
            }

            TuiEvent::CommandRegistered {
                ref name,
                ref source,
                ..
            } => {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("  [command] New command registered: /{name} ({source})"),
                        LineKind::System,
                    )));
            }
        }
    }

    /// Handle a single character keypress (convenience method for tests and
    /// callers that synthesize character input).
    pub fn handle_char(&mut self, c: char) {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let key = KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        self.handle_key(key);
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        self.error_message = None;

        // Menu driver takes highest priority when active — captures all keys.
        if self.menu_driver.is_active() {
            if let Some(menu_result) = self.menu_driver.handle_key(key) {
                match menu_result {
                    crate::menu::MenuResult::Done(payload) => match payload {
                        crate::menu::MenuPayload::Selected(text) => {
                            self.input_bar.insert_str(&text);
                        }
                        crate::menu::MenuPayload::KeyValue { key: k, value: v } => {
                            self.mutations
                                .push(RenderMutation::CommitLine(StyledLine::new(
                                    format!("  set {k} = {v}"),
                                    LineKind::System,
                                )));
                        }
                        crate::menu::MenuPayload::Empty => {}
                    },
                    crate::menu::MenuResult::Cancel | crate::menu::MenuResult::Continue => {}
                }
                self.push_prompt_update();
                return;
            }
        }

        // Shift+Tab — cycle mode (PRD 66).
        if key.code == crossterm::event::KeyCode::BackTab {
            let from_name = self.mode_manager.active().name.clone();
            let new_mode = self.mode_manager.cycle();
            let new_name = new_mode.name.clone();
            self.mode_notification = Some(format!("✦ Switched to {} mode", new_name));
            let sig = xaft_agent::XaftModeChanged {
                from_name,
                to_name: new_name,
                to_label: self.mode_manager.active().label.clone(),
                to_colour: self.mode_manager.active().colour.as_str().to_string(),
            };
            emit_signal_if_bus(self.signal_bus.clone(), sig);
            self.push_prompt_update();
            return;
        }

        // F3 escape-confirmation dialog keys take priority over
        // everything else (input bar, approval queue, etc.). The dialog
        // is modal.
        if self.escape_dialog.is_some() {
            if self.handle_escape_dialog_key(key) {
                return;
            }
            // Swallow other keys while the dialog is open.
            return;
        }

        // Unified trigger-dispatch block.
        // Runs before approval-queue handling.
        if self.active_trigger.is_some() {
            let handled = self.handle_trigger_key(key);
            if handled {
                self.push_prompt_update();
                return;
            }
            // Fall through for unhandled keys (e.g. printable chars).
        }

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

        // Ctrl+B — detach pipeline to background / re-attach.
        // Checked before the input bar so emacs Ctrl+B (cursor-left) doesn't
        // intercept it when the user means background-mode.
        if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.handle_ctrl_b();
            return;
        }

        // Digit-key selection while awaiting background pipeline choice.
        if self.awaiting_bg_select {
            if let KeyCode::Char(c) = key.code {
                if c.is_ascii_digit() && c != '0' {
                    let digit = (c as usize) - ('0' as usize);
                    if digit <= self.background_entries.len() {
                        self.awaiting_bg_select = false;
                        self.reattach_pipeline(digit - 1);
                    } else {
                        self.awaiting_bg_select = false;
                        self.mutations
                            .push(RenderMutation::CommitLine(StyledLine::new(
                                format!("  Invalid selection: {}", c),
                                LineKind::System,
                            )));
                    }
                    return;
                }
            }
            // Any other key cancels selection mode.
            self.awaiting_bg_select = false;
        }

        // InputBar: capture printable chars and editing keys. Keys the bar
        // doesn't handle (Ctrl+C, Ctrl+Q, etc.) fall through to the next match.
        let consumed = match self.input_bar.handle_key(key) {
            InputAction::Submit(msg) => {
                self.mutations
                    .push(RenderMutation::CommitLine(StyledLine::new(
                        format!("❯ {msg}"),
                        LineKind::UserMessage,
                    )));
                self.handle_submit(msg);
                true
            }
            InputAction::BufferChanged | InputAction::CursorMoved => {
                self.refresh_trigger();
                self.push_prompt_update();
                true
            }
            InputAction::NoOp => false,
        };
        if consumed {
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || self.task_done
                    || self.phase == WorkflowPhase::Error
                {
                    let running_bg = self
                        .background_entries
                        .iter()
                        .filter(|e| e.status == BackgroundStatus::Running)
                        .count();
                    if running_bg > 0 && !self.quit_confirmed_with_bg {
                        self.quit_confirmed_with_bg = true;
                        self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
                            format!(
                                "  ⚠ {} background pipeline(s) still running and will be cancelled. Press q again to quit.",
                                running_bg
                            ),
                            LineKind::System,
                        )));
                    } else {
                        self.should_quit = true;
                    }
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

    /// Consume and return the one-shot mode switch notification (PRD 66).
    pub fn take_mode_notification(&mut self) -> Option<String> {
        self.mode_notification.take()
    }

    /// Build and push an `UpdatePrompt` render mutation.
    fn push_prompt_update(&mut self) {
        let prompt = build_prompt(self);
        self.mutations.push(RenderMutation::UpdatePrompt(prompt));
    }

    /// Open a menu widget by command name using the `CommandMenuRegistry`.
    ///
    /// Returns `true` if a factory was found and the menu was opened; `false`
    /// if no factory is registered for `command`.
    pub fn open_menu_by_name(&mut self, command: &str) -> bool {
        let factory = match self.menu_registry.get(command) {
            Some(f) => f,
            None => return false,
        };
        let ctx = crate::menu::CommandMenuContext {
            config: Arc::new(self.xaft_config.as_ref().clone()),
            working_dir: self.working_dir.clone(),
            session_id: self.session.as_ref().map(|s| s.id.to_string()),
        };
        let widget = factory(ctx);
        self.menu_driver.open(widget);
        self.push_prompt_update();
        true
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
            texts.iter().any(|t| t.contains("Planner")),
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
            session: xaft_runtime::session::AgentSession::new(
                "test",
                std::path::PathBuf::from("."),
                "default".into(),
                "claude-3".into(),
            ),
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
        s.input_bar.set_text("hello world");
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        let texts = commit_texts(&s);
        assert!(texts.iter().any(|t| t.contains("hello world")));
        assert!(
            s.input_bar.is_empty(),
            "buffer should be cleared after enter"
        );
    }

    #[test]
    fn backspace_pops_input_and_updates_prompt() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut s = make_state();
        s.input_bar.set_text("hello");
        s.handle_event(TuiEvent::Key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        assert_eq!(s.input_bar.text(), "hell");
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
