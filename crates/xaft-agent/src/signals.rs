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

/// Emitted when the dynamic handoff orchestrator transfers control from one
/// agent to another.
///
/// Subscribe via the shared `SignalBus` to observe handoff events in real time:
///
/// ```rust,ignore
/// bus.on::<XaftAgentHandoff>(|e| {
///     println!("handoff: {} → {} ({})", e.from_agent, e.to_agent, e.summary);
/// }).await;
/// ```
#[derive(Clone, Debug)]
pub struct XaftAgentHandoff {
    /// Name of the agent that handed off.
    pub from_agent: String,
    /// Name of the agent receiving the handoff.
    pub to_agent: String,
    /// Summary / reason supplied to the handoff tool.
    pub summary: String,
}

/// Emitted for each incremental text token produced by an LLM during streaming.
///
/// Consumed by the TUI to update the live streaming indicator so tokens appear
/// character-by-character rather than only after the full turn completes.
#[derive(Clone, Debug)]
pub struct XaftStreamToken {
    /// Agent that produced this token.
    pub agent_name: String,
    /// Incremental text delta from the LLM.
    pub token: String,
}

/// Emitted whenever the TUI input bar buffer changes (per keystroke / paste).
///
/// Diagnostic / metrics only — never consumed for state mutation. The TUI
/// does not route any workflow transitions through this signal.
#[derive(Debug, Clone)]
pub struct XaftInputBufferEdited {
    /// Logical line count after the edit.
    pub line_count: usize,
    /// Character count (Unicode scalar values) after the edit.
    pub char_count: usize,
    /// Source of the edit — useful for metrics and `/cost` panels.
    pub source: InputEditSource,
}

/// Provenance of an input-bar edit, used for telemetry and forward-compat
/// with F23 (input history) and F24 (external editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEditSource {
    /// Single key press (insert char, backspace, cursor move, etc.).
    KeyPress,
    /// Bracketed-paste payload.
    Paste,
    /// Programmatic insertion (F24 external editor).
    ExternalEditor,
    /// History recall (F23) — Up arrow on empty buffer.
    HistoryRecall,
}

/// Emitted when the user submits a message from the input bar (Enter key).
///
/// Captured BEFORE the runtime sees the message. Distinguishes single-line
/// from multi-line submissions so the `/cost` and `/doctor` panels can show
/// the distribution of prompt shapes.
#[derive(Debug, Clone)]
pub struct XaftUserMessageSubmitted {
    /// Logical line count of the submitted message.
    pub line_count: usize,
    /// Character count (Unicode scalar values) of the submitted message.
    pub char_count: usize,
    /// `true` when the submission spanned more than one line.
    pub had_multi_line: bool,
}

// ── F3 @-mention signals ────────────────────────────────────────────────────
//
// These five signals are emitted by the TUI's submit-time resolver and by
// the escape confirmation dialog. They are *observational* only — they
// never mutate workflow state. The `SignalBus` broadcasts them to any
// subscribed observer (e.g. the audit log, the `/cost` panel, the
// telemetry exporter).

/// Emitted after `MentionResolver::expand()` finishes, before any
/// escape-confirmation dialog is shown. Carries the number of mentions
/// found, how many resolved, how many produced warnings, and how many
/// escaped the workspace (and therefore require user approval).
///
/// The TUI emits this *every* time a submission is parsed, even if there
/// were no mentions at all — the `mention_count` field distinguishes the
/// two cases.
#[derive(Debug, Clone)]
pub struct XaftMentionsResolved {
    /// Number of `@<path>` tokens the parser found (0 means no mentions).
    pub mention_count: usize,
    /// Number of mentions that resolved to a `FileRef` block successfully.
    pub resolved_count: usize,
    /// Number of mentions that produced a warning (file not found, too
    /// large, binary, etc.). Sum of `resolved_count + warning_count`
    /// may be less than `mention_count` when escape mentions were
    /// rejected by `escape_policy = "never"`.
    pub warning_count: usize,
    /// Number of mentions classified as workspace escapes (per PRD 30a
    /// §5.2). Zero when all paths were workspace-relative.
    pub escape_count: usize,
    /// Total bytes across all resolved files (escape + workspace).
    pub total_bytes: u64,
}

/// Emitted for each successfully resolved `FileRef` block, after the
/// escape confirmation dialog (if any) has been approved. One signal per
/// block — listeners get a clean per-file view of the audit log.
///
/// The TUI emits this regardless of whether the path was an escape or
/// workspace-relative; the `is_escape` field distinguishes the two.
#[derive(Debug, Clone)]
pub struct XaftFileRefAttached {
    /// The literal path the user typed.
    pub path: String,
    /// Canonical absolute path on disk (or the workspace-relative path
    /// when not an escape).
    pub canonical_path: String,
    /// Byte size of the file (post-truncation if truncated, otherwise
    /// the original on-disk size).
    pub byte_size: u64,
    /// Line count of the file (0 for images, post-truncation for text).
    pub line_count: u64,
    /// SHA-256 of the bytes the resolver actually sent to the LLM.
    /// For truncated files this is the sha256 of the truncated body,
    /// NOT the on-disk full file (documented limitation in PRD 30b
    /// §11.1).
    pub sha256: String,
    /// `true` when the path escaped the workspace.
    pub is_escape: bool,
}

/// Emitted for each mention that failed to resolve (file not found, too
/// large, binary, etc.). The TUI also inlines the literal `@<path>` text
/// in the message it sends to the LLM so the user can see what was
/// skipped, but this signal is the structured counterpart for the audit
/// log.
#[derive(Debug, Clone)]
pub struct XaftFileRefNotFound {
    /// The literal path the user typed.
    pub path: String,
    /// The reason resolution failed.
    pub reason: String,
}

/// Emitted when the user approves an escape confirmation dialog. Carries
/// the escape mentions that were approved, plus whether the approval is
/// one-shot (only this submission) or session-wide (all future escape
/// mentions skip the dialog for the rest of the session).
///
/// The audit log records every approved escape mention with the absolute
/// path and the SHA-256 of the file content. This signal is the
/// observable counterpart — the actual mutation happens through the
/// regular `RunRequest` dispatch path, never through the signal itself.
#[derive(Debug, Clone)]
pub struct XaftEscapeMentionApproved {
    /// Escape mentions that were approved (always at least one).
    pub tokens: Vec<EscapeSignalEntry>,
    /// `true` when the user pressed `A` to approve for the rest of the
    /// session. `false` for a one-shot approval (`a` / `Enter`).
    pub session_wide: bool,
}

/// Emitted when the user cancels the escape confirmation dialog
/// (`c` / `Esc`). The input bar is restored and no message is sent.
#[derive(Debug, Clone)]
pub struct XaftEscapeMentionDenied {
    /// Escape mentions that were in the dialog (always at least one).
    pub tokens: Vec<EscapeSignalEntry>,
    /// The reason the dialog was closed. `"cancel"` for `c`/`Esc`,
    /// `"session_wide_revoked"` is reserved for a future "revoke all
    /// approvals" command and is not emitted in v0.2.
    pub reason: String,
}

/// One escape mention in a `XaftEscapeMentionApproved` /
/// `XaftEscapeMentionDenied` signal. Mirrors [`agtrs_runtime::transport::EscapeInfo`]
/// for the audit log, but adds the raw token (the literal text the user
/// typed) so the log can distinguish `@/etc/hosts` from
/// `@/etc/hosts  ` (trailing space).
#[derive(Debug, Clone)]
pub struct EscapeSignalEntry {
    /// Literal text the user typed (after `@`).
    pub raw_token: String,
    /// Classified reason for the escape.
    pub reason: String,
    /// Canonicalised absolute path on disk.
    pub absolute_path: String,
    /// File size in bytes.
    pub byte_size: u64,
    /// Number of `..` segments (0 for non-traversal).
    pub depth: u32,
}

// ── Background pipeline signals ───────────────────────────────────────────────

/// Emitted when a background pipeline finishes (success or failure).
#[derive(Debug, Clone)]
pub struct XaftBackgroundPipelineComplete {
    /// Unique pipeline ID assigned at detach time.
    pub id: u64,
    /// First ~60 chars of the user prompt.
    pub task_summary: String,
    /// `true` if the pipeline completed successfully.
    pub success: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: f64,
}

/// Emitted when a foreground pipeline is detached to background with `Ctrl+B`.
#[derive(Debug, Clone)]
pub struct XaftPipelineDetached {
    /// Unique pipeline ID.
    pub id: u64,
    /// First ~60 chars of the user prompt.
    pub task_summary: String,
}

/// Emitted when a background pipeline is re-attached to foreground with `Ctrl+B`.
#[derive(Debug, Clone)]
pub struct XaftPipelineReattached {
    /// Unique pipeline ID.
    pub id: u64,
    /// Number of buffered mutations replayed into the transcript.
    pub buffered_mutations: usize,
}

// ── Config patch signal ───────────────────────────────────────────────────────

/// Emitted when the user applies a config patch via the TUI editor.
#[derive(Debug, Clone)]
pub struct XaftConfigPatched {
    /// Dotted key path (e.g. `"agent.default.max_turns"`).
    pub key: String,
    /// String representation of the old value.
    pub old_value: String,
    /// String representation of the new value.
    pub new_value: String,
    /// Always `"session"` for TUI-sourced patches.
    pub layer: String,
}

// ── Compaction signals ────────────────────────────────────────────────────────

/// Emitted when context compaction completes (auto or manual).
#[derive(Debug, Clone)]
pub struct XaftContextCompacted {
    /// The name of the agent whose conversation was compacted.
    pub agent_name: String,
    /// Number of original messages replaced by the summary block.
    pub messages_removed: usize,
    /// Total character length of the messages that were removed.
    pub chars_removed: usize,
    /// Character length of the generated summary text.
    pub summary_chars: usize,
    /// Rough token estimate of space freed: `chars_removed / 4`.
    pub tokens_saved_estimate: u64,
}

/// Emitted by the TUI `/compact` command to request in-flight compaction.
#[derive(Debug, Clone)]
pub struct XaftCompactRequested {
    /// `None` = compact all agents; `Some(name)` = specific agent only.
    pub agent_name: Option<String>,
}
