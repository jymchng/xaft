//! Approval system — risk classification, queue, history, auto-approve rules.
//!
//! # Flow
//!
//! ```text
//! Tool with requires_confirmation=true
//!   → executor emits ToolPendingApproval signal
//!   → TuiApprovalGate::request() creates oneshot
//!   → EventBridge forwards as TuiEvent::ToolPendingApproval (with RiskLevel)
//!   → AppState::approval_queue.push() may auto-approve (Low/Medium) or enqueue (High/Critical)
//!   → ApprovalWidget renders modal / batch view / history
//!   → User presses [a/r/e/A/R/s/u/c/…]
//!   → TuiApprovalGate::respond() wakes the executor
//! ```

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ── Risk level ────────────────────────────────────────────────────────────────

/// Risk classification for a tool call.
///
/// Auto-approve policy: Low → always approve; Medium → approve unless in strict
/// mode; High/Critical → always gate to user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskLevel {
    /// Read-only, non-destructive. Auto-approved by default.
    Low = 0,
    /// File edits, build commands. Auto-approved unless strict mode.
    Medium = 1,
    /// Destructive commands, critical-file edits. Always gates.
    High = 2,
    /// Irreversible / system-wide effects. Always gates with prominent warning.
    Critical = 3,
}

impl RiskLevel {
    /// Short text label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        }
    }

    /// Fraction of the risk gauge to fill (0.0 – 1.0).
    pub fn gauge_fraction(self) -> f32 {
        match self {
            Self::Low => 0.25,
            Self::Medium => 0.50,
            Self::High => 0.75,
            Self::Critical => 1.00,
        }
    }

    /// Gauge blocks (out of 8).
    pub fn gauge_blocks(self) -> usize {
        match self {
            Self::Low => 2,
            Self::Medium => 4,
            Self::High => 6,
            Self::Critical => 8,
        }
    }

    /// Compute risk from tool name + JSON input.
    pub fn from_tool_call(tool_name: &str, input: &serde_json::Value) -> Self {
        match tool_name {
            // Always safe
            "read_file" | "list_files" | "grep" | "git_status" | "git_log" | "git_diff"
            | "git_blame" => Self::Low,

            // Shell commands — inspect command
            "bash_exec" => {
                let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                classify_bash_risk(cmd)
            }

            // File edits
            "write_file" | "edit_file" => {
                let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if is_critical_path(path) {
                    Self::High
                } else {
                    Self::Medium
                }
            }

            // Git write ops
            "git_commit" | "git_stage" => Self::Medium,
            "git_restore" => Self::High,

            // Default
            _ => Self::Medium,
        }
    }
}

/// Classify risk from a shell command string.
fn classify_bash_risk(cmd: &str) -> RiskLevel {
    // Critical: destructive filesystem ops
    if cmd.contains("rm -rf")
        || cmd.contains("rm -fr")
        || cmd.contains("dd ")
        || cmd.contains("mkfs")
        || cmd.contains("fdisk")
        || cmd.contains("> /dev/")
    {
        return RiskLevel::Critical;
    }
    // High: privilege escalation, permissions, destructive rm
    if cmd.starts_with("sudo ")
        || cmd.contains("sudo ")
        || cmd.contains("chmod")
        || cmd.contains("chown")
        || cmd.starts_with("rm ")
        || cmd.contains(" rm ")
        || cmd.contains("kill ")
        || cmd.contains("pkill ")
    {
        return RiskLevel::High;
    }
    // Low: common dev commands
    if cmd.starts_with("cargo ")
        || cmd.starts_with("git ")
        || cmd.starts_with("npm test")
        || cmd.starts_with("pytest")
        || cmd.starts_with("python ")
        || cmd.starts_with("echo ")
        || cmd.starts_with("cat ")
        || cmd.starts_with("ls ")
        || cmd.starts_with("grep ")
        || cmd.starts_with("find ")
        || cmd.starts_with("head ")
        || cmd.starts_with("tail ")
    {
        return RiskLevel::Low;
    }
    RiskLevel::Medium
}

/// Return true for paths that have elevated sensitivity.
fn is_critical_path(path: &str) -> bool {
    const CRITICAL: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        ".env",
        ".gitignore",
        "Dockerfile",
        "docker-compose",
        "Makefile",
        "justfile",
        "main.rs",
        "main.go",
        "index.ts",
        "index.js",
        "config",
        "secrets",
        "credentials",
        ".github",
        "CI",
    ];
    CRITICAL.iter().any(|p| path.contains(p))
}

// ── Approval result ───────────────────────────────────────────────────────────

/// The user's decision for a pending approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Approved (execute the tool).
    Approved,
    /// Rejected (cancel the tool call).
    Rejected,
    /// Skipped (defer — not yet decided; tool waits).
    Skipped,
    /// Auto-approved by rule.
    AutoApproved { rule: String },
}

impl ApprovalDecision {
    /// Short display string.
    pub fn label(&self) -> &str {
        match self {
            Self::Approved => "✓ Approved",
            Self::Rejected => "✗ Rejected",
            Self::Skipped => "○ Skipped",
            Self::AutoApproved { .. } => "✓ Auto",
        }
    }

    /// True when the tool should proceed.
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved | Self::AutoApproved { .. })
    }
}

// ── Pending approval ──────────────────────────────────────────────────────────

/// A single pending approval request in the queue.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    /// Correlation ID matching the oneshot channel in `TuiApprovalGate`.
    pub tool_use_id: String,
    /// The agent that requested this tool call.
    pub agent_run_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Full JSON input.
    pub input: serde_json::Value,
    /// Short truncated preview for the queue list.
    pub input_preview: String,
    /// Computed risk level.
    pub risk: RiskLevel,
    /// When this request arrived.
    pub arrived_at: Instant,
    /// Whether this item has been decided (but not yet removed from queue).
    pub decided: Option<ApprovalDecision>,
}

impl PendingApproval {
    /// Build from the raw event fields, computing risk automatically.
    pub fn new(
        tool_use_id: impl Into<String>,
        agent_run_id: impl Into<String>,
        tool_name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        let tool_name = tool_name.into();
        let risk = RiskLevel::from_tool_call(&tool_name, &input);
        let input_preview = build_preview(&tool_name, &input, 60);
        Self {
            tool_use_id: tool_use_id.into(),
            agent_run_id: agent_run_id.into(),
            tool_name,
            input,
            input_preview,
            risk,
            arrived_at: Instant::now(),
            decided: None,
        }
    }

    /// Elapsed milliseconds since arrival.
    pub fn age_ms(&self) -> u64 {
        self.arrived_at.elapsed().as_millis() as u64
    }
}

// ── Approval history record ───────────────────────────────────────────────────

/// A completed approval decision stored in session history.
#[derive(Debug, Clone)]
pub struct ApprovalRecord {
    pub tool_use_id: String,
    pub tool_name: String,
    pub risk: RiskLevel,
    pub decision: ApprovalDecision,
    pub decided_at: Instant,
    /// Age in seconds at time of display (updated lazily).
    pub age_label: String,
}

impl ApprovalRecord {
    /// Formatted age: "30s", "2m", "1h".
    pub fn format_age(age_secs: u64) -> String {
        if age_secs < 60 {
            format!("{age_secs}s")
        } else if age_secs < 3600 {
            format!("{}m", age_secs / 60)
        } else {
            format!("{}h", age_secs / 3600)
        }
    }
}

// ── Auto-approve rules ────────────────────────────────────────────────────────

/// A single auto-approve rule.
#[derive(Debug, Clone)]
pub struct AutoApproveRule {
    /// Tool name pattern (exact match or `*` suffix wildcard, e.g. `"cargo*"`).
    pub tool_pattern: String,
    /// Maximum risk level that this rule approves.
    pub max_risk: RiskLevel,
    /// Optional command substring match (for `bash_exec`).
    pub command_pattern: Option<String>,
    /// Optional path prefix match (for file tools).
    pub path_pattern: Option<String>,
    /// Description shown in TUI next to auto-approved items.
    pub label: String,
}

impl AutoApproveRule {
    /// True if this rule matches the given tool call.
    pub fn matches(&self, tool_name: &str, input: &serde_json::Value, risk: RiskLevel) -> bool {
        if risk > self.max_risk {
            return false;
        }
        if !pattern_matches(&self.tool_pattern, tool_name) {
            return false;
        }
        if let Some(ref cmd_pat) = self.command_pattern {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if !pattern_matches(cmd_pat, cmd) {
                return false;
            }
        }
        if let Some(ref path_pat) = self.path_pattern {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.starts_with(path_pat.trim_end_matches('*')) {
                return false;
            }
        }
        true
    }
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    pattern == value
}

/// Collection of auto-approve rules (evaluated top-to-bottom; first match wins).
#[derive(Debug, Clone, Default)]
pub struct AutoApproveConfig {
    pub rules: Vec<AutoApproveRule>,
    /// When true, Low-risk tools are auto-approved even with no matching rule.
    pub auto_low: bool,
    /// When true, Medium-risk tools are auto-approved even with no matching rule.
    pub auto_medium: bool,
}

impl AutoApproveConfig {
    /// Built-in sensible defaults.
    pub fn default_safe() -> Self {
        Self {
            auto_low: true,
            auto_medium: false,
            rules: vec![
                AutoApproveRule {
                    tool_pattern: "bash_exec".into(),
                    max_risk: RiskLevel::Low,
                    command_pattern: Some("cargo*".into()),
                    path_pattern: None,
                    label: "cargo commands".into(),
                },
                AutoApproveRule {
                    tool_pattern: "bash_exec".into(),
                    max_risk: RiskLevel::Low,
                    command_pattern: Some("git diff*".into()),
                    path_pattern: None,
                    label: "git diff".into(),
                },
            ],
        }
    }

    /// Check if a tool call should be auto-approved.
    /// Returns `Some(rule_label)` if auto-approved, `None` if manual gate required.
    pub fn check(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        risk: RiskLevel,
    ) -> Option<String> {
        // High/Critical never auto-approve
        if risk >= RiskLevel::High {
            return None;
        }
        // Check explicit rules first
        for rule in &self.rules {
            if rule.matches(tool_name, input, risk) {
                return Some(rule.label.clone());
            }
        }
        // Fallback to global risk thresholds
        if risk == RiskLevel::Low && self.auto_low {
            return Some("auto (low risk)".into());
        }
        if risk == RiskLevel::Medium && self.auto_medium {
            return Some("auto (medium risk)".into());
        }
        None
    }
}

// ── Approval queue ────────────────────────────────────────────────────────────

/// The live approval queue and session history.
///
/// Lives in `AppState` and is updated by keyboard events and incoming signals.
#[derive(Debug, Default)]
pub struct ApprovalQueue {
    /// Pending approvals awaiting a decision (newest last).
    pub pending: VecDeque<PendingApproval>,
    /// Session approval history (newest last, max 200).
    pub history: VecDeque<ApprovalRecord>,
    /// Index of the focused item in `pending` (0 = oldest).
    pub focused_idx: usize,
    /// Auto-approve configuration.
    pub config: AutoApproveConfig,
    /// Whether the history view is currently shown.
    pub show_history: bool,
    /// Session stats
    pub total_approved: u32,
    pub total_rejected: u32,
    pub total_auto: u32,
}

impl ApprovalQueue {
    pub fn new(config: AutoApproveConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    /// Add a new pending approval.  Returns `Some(decision)` if auto-approved;
    /// `None` if the item was queued and requires manual decision.
    pub fn push(
        &mut self,
        tool_use_id: impl Into<String>,
        agent_run_id: impl Into<String>,
        tool_name: impl Into<String>,
        input: serde_json::Value,
    ) -> Option<ApprovalDecision> {
        let item = PendingApproval::new(tool_use_id, agent_run_id, &tool_name.into(), input);
        // Check auto-approve
        if let Some(rule) = self.config.check(&item.tool_name, &item.input, item.risk) {
            let decision = ApprovalDecision::AutoApproved { rule: rule.clone() };
            self.record_decision(&item, decision.clone());
            return Some(decision);
        }
        self.pending.push_back(item);
        None
    }

    /// True when there are items requiring a manual decision.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The currently focused pending item (for the single-item dialog).
    pub fn focused(&self) -> Option<&PendingApproval> {
        self.pending.get(self.focused_idx)
    }

    /// Resolve the focused item with `decision`.
    /// Returns the `tool_use_id` of the resolved item (caller must call `TuiApprovalGate::respond`).
    pub fn resolve_focused(&mut self, decision: ApprovalDecision) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let idx = self.focused_idx.min(self.pending.len() - 1);
        let item = self.pending.remove(idx)?;
        let id = item.tool_use_id.clone();
        self.record_decision(&item, decision);
        // Adjust focused index
        if self.focused_idx >= self.pending.len() && self.focused_idx > 0 {
            self.focused_idx -= 1;
        }
        Some(id)
    }

    /// Approve all pending items with risk ≤ `max_risk`.
    /// Returns list of (tool_use_id, decision) for the caller to forward to the gate.
    pub fn approve_all_up_to(&mut self, max_risk: RiskLevel) -> Vec<(String, ApprovalDecision)> {
        let to_approve: Vec<usize> = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, a)| a.risk <= max_risk)
            .map(|(i, _)| i)
            .collect();

        let mut results = Vec::new();
        // Remove in reverse order to keep indices valid
        for idx in to_approve.iter().rev() {
            if let Some(item) = self.pending.remove(*idx) {
                let decision = ApprovalDecision::Approved;
                results.push((item.tool_use_id.clone(), decision.clone()));
                self.record_decision(&item, decision);
            }
        }
        self.focused_idx = 0;
        results
    }

    /// Reject all pending items.
    pub fn reject_all(&mut self) -> Vec<String> {
        let items: Vec<PendingApproval> = self.pending.drain(..).collect();
        let ids: Vec<String> = items.iter().map(|a| a.tool_use_id.clone()).collect();
        for item in &items {
            self.record_decision(item, ApprovalDecision::Rejected);
        }
        self.focused_idx = 0;
        ids
    }

    /// Undo the last manual decision (if the gate is still responding).
    /// Returns the tool_use_id that was undone, for caller to re-enqueue.
    pub fn undo_last(&mut self) -> Option<String> {
        // Find last non-auto decision
        loop {
            let is_auto = match self.history.back() {
                None => return None,
                Some(r) => matches!(r.decision, ApprovalDecision::AutoApproved { .. }),
            };
            let record = self.history.pop_back().unwrap();
            if !is_auto {
                // Adjust stats (best-effort)
                match record.decision {
                    ApprovalDecision::Approved => {
                        self.total_approved = self.total_approved.saturating_sub(1)
                    }
                    ApprovalDecision::Rejected => {
                        self.total_rejected = self.total_rejected.saturating_sub(1)
                    }
                    _ => {}
                }
                return Some(record.tool_use_id);
            }
        }
    }

    /// Focus next pending item.
    pub fn focus_next(&mut self) {
        if !self.pending.is_empty() {
            self.focused_idx = (self.focused_idx + 1) % self.pending.len();
        }
    }

    /// Focus previous pending item.
    pub fn focus_prev(&mut self) {
        if !self.pending.is_empty() {
            self.focused_idx = if self.focused_idx == 0 {
                self.pending.len() - 1
            } else {
                self.focused_idx - 1
            };
        }
    }

    fn record_decision(&mut self, item: &PendingApproval, decision: ApprovalDecision) {
        match &decision {
            ApprovalDecision::Approved => self.total_approved += 1,
            ApprovalDecision::Rejected => self.total_rejected += 1,
            ApprovalDecision::AutoApproved { .. } => self.total_auto += 1,
            _ => {}
        }
        let record = ApprovalRecord {
            tool_use_id: item.tool_use_id.clone(),
            tool_name: item.tool_name.clone(),
            risk: item.risk,
            decision,
            decided_at: Instant::now(),
            age_label: "now".into(),
        };
        self.history.push_back(record);
        if self.history.len() > 200 {
            self.history.pop_front();
        }
    }
}

// ── Input preview helpers ─────────────────────────────────────────────────────

/// Build a short single-line preview of the tool input.
pub fn build_preview(tool_name: &str, input: &serde_json::Value, max_len: usize) -> String {
    let raw = match tool_name {
        "bash_exec" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| format!("$ {s}"))
            .unwrap_or_else(|| "bash".into()),
        "write_file" | "edit_file" | "read_file" => input
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| p.to_string())
            .unwrap_or_else(|| tool_name.into()),
        "grep" => {
            let pat = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            format!("/{pat}/")
        }
        _ => {
            // Generic: first string value
            if let Some(obj) = input.as_object() {
                obj.values()
                    .find_map(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| tool_name.into())
            } else {
                tool_name.into()
            }
        }
    };
    if raw.len() > max_len {
        format!("{}…", &raw[..max_len.saturating_sub(1)])
    } else {
        raw
    }
}

/// Returns `true` when the `write_file` input has no prior content — i.e., it's a new file.
pub fn is_new_file(input: &serde_json::Value) -> bool {
    // The tool might include an `existing: bool` flag, or we check content absence heuristically.
    input
        .get("existing")
        .and_then(|v| v.as_bool())
        .map(|e| !e)
        .unwrap_or(false)
}

/// Context passed to risk computation for richer classification.
#[derive(Debug, Clone, Default)]
pub struct ApprovalContext {
    /// Agent that triggered the tool call.
    pub agent_id: String,
    /// Pre-computed `is_new_file` flag (avoids re-parsing).
    pub is_new_file: bool,
}

impl RiskLevel {
    /// Compute risk using an explicit context (e.g., is_new_file already known).
    pub fn from_tool_call_with_context(
        tool_name: &str,
        input: &serde_json::Value,
        ctx: &ApprovalContext,
    ) -> Self {
        match tool_name {
            "read_file" | "list_files" | "grep" | "git_status" | "git_log" | "git_diff"
            | "git_blame" => Self::Low,

            "bash_exec" => {
                let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                classify_bash_risk(cmd)
            }

            "write_file" | "edit_file" => {
                let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if is_critical_path(path) {
                    Self::High
                } else if ctx.is_new_file {
                    Self::Medium // New file creation is at most Medium
                } else {
                    Self::Medium
                }
            }

            "git_commit" | "git_stage" => Self::Medium,
            "git_restore" => Self::High,
            _ => Self::Medium,
        }
    }
}

// ── ToolPreview ───────────────────────────────────────────────────────────────

/// Structured, tool-specific preview of a pending tool call.
///
/// Used by `ApprovalWidget` to render rich per-tool previews in the modal:
/// boxed bash commands, diff hunks for file edits, numbered file content, etc.
#[derive(Debug, Clone)]
pub enum ToolPreview {
    /// `bash_exec` — command + context
    Bash {
        command: String,
        working_dir: String,
        timeout_secs: Option<u64>,
    },
    /// `edit_file` — path + old/new lines for inline diff
    FileEdit {
        path: String,
        old_lines: Vec<String>,
        new_lines: Vec<String>,
    },
    /// `write_file` — path + content preview
    FileWrite {
        path: String,
        content_preview: Vec<String>,
        total_bytes: usize,
        is_new: bool,
    },
    /// `read_file`
    FileRead { path: String },
    /// `web_fetch` / HTTP tools
    WebFetch { url: String, method: String },
    /// Generic JSON preview (all other tools)
    Generic { lines: Vec<String> },
}

impl ToolPreview {
    /// Build a `ToolPreview` from raw tool call data.
    pub fn from_input(tool_name: &str, input: &serde_json::Value) -> Self {
        match tool_name {
            "bash_exec" => Self::Bash {
                command: input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                working_dir: input
                    .get("working_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".")
                    .to_string(),
                timeout_secs: input.get("timeout_secs").and_then(|v| v.as_u64()),
            },

            "edit_file" => {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let old_lines = input
                    .get("old_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .lines()
                    .map(|l| l.to_string())
                    .collect();
                let new_lines = input
                    .get("new_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .lines()
                    .map(|l| l.to_string())
                    .collect();
                Self::FileEdit {
                    path,
                    old_lines,
                    new_lines,
                }
            }

            "write_file" => {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let total_bytes = content.len();
                let is_new = is_new_file(input);
                let content_preview: Vec<String> =
                    content.lines().take(15).map(|l| l.to_string()).collect();
                Self::FileWrite {
                    path,
                    content_preview,
                    total_bytes,
                    is_new,
                }
            }

            "read_file" => Self::FileRead {
                path: input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
            },

            "web_fetch" | "http_request" | "fetch_url" => Self::WebFetch {
                url: input
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                method: input
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET")
                    .to_string(),
            },

            _ => {
                let json = serde_json::to_string_pretty(input).unwrap_or_default();
                let lines: Vec<String> = json.lines().take(12).map(|l| l.to_string()).collect();
                Self::Generic { lines }
            }
        }
    }

    /// Approximate number of terminal rows needed to render this preview.
    pub fn content_height(&self) -> u16 {
        match self {
            Self::Bash { .. } => 5,
            Self::FileEdit {
                old_lines,
                new_lines,
                ..
            } => {
                let diff_lines = old_lines.len().max(new_lines.len()).min(10);
                4 + diff_lines as u16
            }
            Self::FileWrite {
                content_preview, ..
            } => 4 + content_preview.len().min(8) as u16,
            Self::FileRead { .. } => 3,
            Self::WebFetch { .. } => 3,
            Self::Generic { lines } => 2 + lines.len().min(8) as u16,
        }
    }

    /// Convert to plain text lines for backward compatibility / headless tests.
    pub fn to_plain_lines(&self) -> Vec<String> {
        match self {
            Self::Bash {
                command,
                working_dir,
                timeout_secs,
            } => {
                let mut out = vec![
                    format!("  Command:   {command}"),
                    format!("  Directory: {working_dir}"),
                ];
                if let Some(t) = timeout_secs {
                    out.push(format!("  Timeout:   {t}s"));
                }
                out
            }
            Self::FileEdit {
                path,
                old_lines,
                new_lines,
            } => vec![
                format!("  File:      {path}"),
                format!(
                    "  Change:    {} → {} lines",
                    old_lines.len(),
                    new_lines.len()
                ),
            ],
            Self::FileWrite {
                path,
                total_bytes,
                is_new,
                ..
            } => vec![
                format!("  File:      {path}"),
                format!(
                    "  Size:      {} bytes{}",
                    total_bytes,
                    if *is_new { " (new file)" } else { "" }
                ),
            ],
            Self::FileRead { path } => vec![format!("  File:      {path}")],
            Self::WebFetch { url, method } => {
                vec![format!("  {method} {url}")]
            }
            Self::Generic { lines } => lines.iter().map(|l| format!("  {l}")).take(5).collect(),
        }
    }
}

/// Build the full tool-specific preview lines for the modal.
///
/// This is the backward-compatible API; prefer [`ToolPreview::from_input`] for
/// rich rendering in the TUI widget.
pub fn tool_preview_lines(tool_name: &str, input: &serde_json::Value) -> Vec<String> {
    match tool_name {
        "bash_exec" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("?");
            let wd = input
                .get("working_dir")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            vec![
                format!("  Command:    {cmd}"),
                format!("  Directory:  {wd}"),
            ]
        }
        "write_file" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let content_len = input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|c| c.len())
                .unwrap_or(0);
            vec![
                format!("  File:       {path}"),
                format!("  Size:       {} bytes", content_len),
            ]
        }
        "edit_file" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let old_len = input
                .get("old_content")
                .and_then(|v| v.as_str())
                .map(|s| s.lines().count())
                .unwrap_or(0);
            let new_len = input
                .get("new_content")
                .and_then(|v| v.as_str())
                .map(|s| s.lines().count())
                .unwrap_or(0);
            vec![
                format!("  File:       {path}"),
                format!("  Change:     {} → {} lines", old_len, new_len),
            ]
        }
        "read_file" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            vec![format!("  File:       {path}")]
        }
        "grep" => {
            let pat = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            let prefix = input
                .get("path_prefix")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            vec![
                format!("  Pattern:    /{pat}/"),
                format!("  Directory:  {prefix}"),
            ]
        }
        _ => {
            // Generic JSON preview, max 5 lines
            let json = serde_json::to_string_pretty(input).unwrap_or_default();
            json.lines().take(5).map(|l| format!("  {l}")).collect()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RiskLevel ──────────────────────────────────────────────────────────────

    #[test]
    fn risk_read_file_is_low() {
        assert_eq!(
            RiskLevel::from_tool_call("read_file", &serde_json::json!({"path": "a.rs"})),
            RiskLevel::Low
        );
    }

    #[test]
    fn risk_cargo_bash_is_low() {
        assert_eq!(
            RiskLevel::from_tool_call("bash_exec", &serde_json::json!({"command": "cargo test"})),
            RiskLevel::Low
        );
    }

    #[test]
    fn risk_rm_rf_is_critical() {
        assert_eq!(
            RiskLevel::from_tool_call(
                "bash_exec",
                &serde_json::json!({"command": "rm -rf target/"})
            ),
            RiskLevel::Critical
        );
    }

    #[test]
    fn risk_sudo_is_high() {
        assert_eq!(
            RiskLevel::from_tool_call(
                "bash_exec",
                &serde_json::json!({"command": "sudo apt install"})
            ),
            RiskLevel::High
        );
    }

    #[test]
    fn risk_write_cargo_toml_is_high() {
        assert_eq!(
            RiskLevel::from_tool_call(
                "write_file",
                &serde_json::json!({"path": "Cargo.toml", "content": "..."})
            ),
            RiskLevel::High
        );
    }

    #[test]
    fn risk_write_normal_file_is_medium() {
        assert_eq!(
            RiskLevel::from_tool_call(
                "write_file",
                &serde_json::json!({"path": "src/lib.rs", "content": "..."})
            ),
            RiskLevel::Medium
        );
    }

    #[test]
    fn risk_ordering() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
        assert!(RiskLevel::High < RiskLevel::Critical);
    }

    #[test]
    fn risk_gauge_fractions_ascending() {
        assert!(RiskLevel::Low.gauge_fraction() < RiskLevel::Medium.gauge_fraction());
        assert!(RiskLevel::Medium.gauge_fraction() < RiskLevel::High.gauge_fraction());
        assert!(RiskLevel::High.gauge_fraction() < RiskLevel::Critical.gauge_fraction());
    }

    // ── Auto-approve ───────────────────────────────────────────────────────────

    #[test]
    fn auto_approve_low_risk_when_enabled() {
        let config = AutoApproveConfig {
            auto_low: true,
            auto_medium: false,
            rules: vec![],
        };
        let result = config.check(
            "read_file",
            &serde_json::json!({"path": "a.rs"}),
            RiskLevel::Low,
        );
        assert!(result.is_some(), "low-risk should auto-approve");
    }

    #[test]
    fn auto_approve_medium_blocked_by_default() {
        let config = AutoApproveConfig::default_safe();
        let result = config.check(
            "write_file",
            &serde_json::json!({"path": "a.rs"}),
            RiskLevel::Medium,
        );
        assert!(
            result.is_none(),
            "medium-risk should not auto-approve by default"
        );
    }

    #[test]
    fn auto_approve_high_never() {
        let config = AutoApproveConfig {
            auto_low: true,
            auto_medium: true,
            rules: vec![],
        };
        let result = config.check(
            "bash_exec",
            &serde_json::json!({"command": "sudo rm"}),
            RiskLevel::High,
        );
        assert!(result.is_none(), "high-risk must never auto-approve");
    }

    #[test]
    fn auto_approve_rule_cargo() {
        let config = AutoApproveConfig::default_safe();
        let result = config.check(
            "bash_exec",
            &serde_json::json!({"command": "cargo test"}),
            RiskLevel::Low,
        );
        assert!(result.is_some());
    }

    #[test]
    fn pattern_exact_match() {
        assert!(pattern_matches("bash_exec", "bash_exec"));
        assert!(!pattern_matches("bash_exec", "write_file"));
    }

    #[test]
    fn pattern_wildcard() {
        assert!(pattern_matches("cargo*", "cargo test"));
        assert!(pattern_matches("cargo*", "cargo build --release"));
        assert!(!pattern_matches("cargo*", "npm test"));
    }

    // ── ApprovalQueue ──────────────────────────────────────────────────────────

    #[test]
    fn queue_push_low_risk_auto_approves() {
        let mut q = ApprovalQueue::new(AutoApproveConfig {
            auto_low: true,
            ..Default::default()
        });
        let result = q.push(
            "tid-1",
            "run-1",
            "read_file",
            serde_json::json!({"path": "a.rs"}),
        );
        assert!(result.is_some(), "low-risk read_file should auto-approve");
        assert!(q.pending.is_empty(), "should not be in pending queue");
        assert_eq!(q.total_auto, 1);
    }

    #[test]
    fn queue_push_high_risk_requires_manual() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default_safe());
        let result = q.push(
            "tid-2",
            "run-1",
            "bash_exec",
            serde_json::json!({"command": "sudo rm -rf /"}),
        );
        assert!(
            result.is_none(),
            "critical risk should require manual approval"
        );
        assert_eq!(q.pending.len(), 1);
    }

    #[test]
    fn queue_resolve_focused_approves() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        q.push(
            "tid-3",
            "run-1",
            "write_file",
            serde_json::json!({"path": "a.rs"}),
        );
        let id = q.resolve_focused(ApprovalDecision::Approved);
        assert_eq!(id, Some("tid-3".into()));
        assert!(q.pending.is_empty());
        assert_eq!(q.total_approved, 1);
        assert_eq!(q.history.len(), 1);
    }

    #[test]
    fn queue_approve_all_up_to_medium() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        // Add medium risk (write) + high risk (sudo)
        q.pending.push_back(PendingApproval::new(
            "t1",
            "r1",
            "write_file",
            serde_json::json!({"path": "a.rs"}),
        ));
        q.pending.push_back(PendingApproval::new(
            "t2",
            "r1",
            "bash_exec",
            serde_json::json!({"command": "sudo rm"}),
        ));
        let approved = q.approve_all_up_to(RiskLevel::Medium);
        assert_eq!(
            approved.len(),
            1,
            "should approve only the medium-risk item"
        );
        assert_eq!(approved[0].0, "t1");
        assert_eq!(q.pending.len(), 1, "high-risk item should remain");
    }

    #[test]
    fn queue_reject_all() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        for i in 0..3 {
            q.pending.push_back(PendingApproval::new(
                format!("t{i}"),
                "r1",
                "write_file",
                serde_json::json!({"path": "a.rs"}),
            ));
        }
        let ids = q.reject_all();
        assert_eq!(ids.len(), 3);
        assert!(q.pending.is_empty());
        assert_eq!(q.total_rejected, 3);
    }

    #[test]
    fn queue_undo_last() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        q.pending.push_back(PendingApproval::new(
            "t1",
            "r1",
            "write_file",
            serde_json::json!({"path": "a.rs"}),
        ));
        q.resolve_focused(ApprovalDecision::Approved);
        assert_eq!(q.total_approved, 1);
        let undone = q.undo_last();
        assert_eq!(undone, Some("t1".into()));
        assert_eq!(q.total_approved, 0);
        assert!(q.history.is_empty());
    }

    #[test]
    fn queue_history_bounded_at_200() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        for i in 0..250 {
            let item = PendingApproval::new(
                format!("t{i}"),
                "r1",
                "write_file",
                serde_json::json!({"path": "a.rs"}),
            );
            q.record_decision(&item, ApprovalDecision::Approved);
        }
        assert_eq!(q.history.len(), 200);
    }

    // ── Preview helpers ────────────────────────────────────────────────────────

    #[test]
    fn preview_bash_shows_command() {
        let p = build_preview(
            "bash_exec",
            &serde_json::json!({"command": "cargo test"}),
            80,
        );
        assert!(p.contains("cargo test"));
    }

    #[test]
    fn preview_write_file_shows_path() {
        let p = build_preview(
            "write_file",
            &serde_json::json!({"path": "src/main.rs", "content": "..."}),
            80,
        );
        assert!(p.contains("src/main.rs"));
    }

    #[test]
    fn preview_truncates_at_max_len() {
        let long = "a".repeat(100);
        let p = build_preview("grep", &serde_json::json!({"pattern": long}), 20);
        assert!(p.len() <= 22); // allow for prefix
    }

    #[test]
    fn tool_preview_lines_bash() {
        let lines = tool_preview_lines(
            "bash_exec",
            &serde_json::json!({"command": "cargo build", "working_dir": "/proj"}),
        );
        assert!(lines.iter().any(|l| l.contains("cargo build")));
        assert!(lines.iter().any(|l| l.contains("/proj")));
    }

    #[test]
    fn tool_preview_lines_write_file() {
        let lines = tool_preview_lines(
            "write_file",
            &serde_json::json!({"path": "src/lib.rs", "content": "fn foo() {}"}),
        );
        assert!(lines.iter().any(|l| l.contains("src/lib.rs")));
        assert!(lines.iter().any(|l| l.contains("bytes")));
    }

    #[test]
    fn decision_is_approved_true() {
        assert!(ApprovalDecision::Approved.is_approved());
        assert!(ApprovalDecision::AutoApproved { rule: "x".into() }.is_approved());
        assert!(!ApprovalDecision::Rejected.is_approved());
        assert!(!ApprovalDecision::Skipped.is_approved());
    }

    // ── ToolPreview ────────────────────────────────────────────────────────────

    #[test]
    fn tool_preview_bash_captures_command() {
        let input = serde_json::json!({
            "command": "cargo test --lib",
            "working_dir": "/project",
            "timeout_secs": 30
        });
        let p = ToolPreview::from_input("bash_exec", &input);
        match p {
            ToolPreview::Bash {
                command,
                working_dir,
                timeout_secs,
            } => {
                assert_eq!(command, "cargo test --lib");
                assert_eq!(working_dir, "/project");
                assert_eq!(timeout_secs, Some(30));
            }
            _ => panic!("expected Bash variant"),
        }
    }

    #[test]
    fn tool_preview_file_edit_captures_lines() {
        let input = serde_json::json!({
            "path": "src/main.rs",
            "old_content": "fn old() {}\n",
            "new_content": "fn new() {}\nfn extra() {}\n"
        });
        let p = ToolPreview::from_input("edit_file", &input);
        match p {
            ToolPreview::FileEdit {
                path,
                old_lines,
                new_lines,
            } => {
                assert_eq!(path, "src/main.rs");
                assert_eq!(old_lines.len(), 1);
                assert_eq!(new_lines.len(), 2);
            }
            _ => panic!("expected FileEdit variant"),
        }
    }

    #[test]
    fn tool_preview_write_file_counts_bytes() {
        let content = "hello world";
        let input = serde_json::json!({
            "path": "out.txt",
            "content": content
        });
        let p = ToolPreview::from_input("write_file", &input);
        match p {
            ToolPreview::FileWrite {
                path, total_bytes, ..
            } => {
                assert_eq!(path, "out.txt");
                assert_eq!(total_bytes, content.len());
            }
            _ => panic!("expected FileWrite variant"),
        }
    }

    #[test]
    fn tool_preview_read_file_captures_path() {
        let input = serde_json::json!({"path": "Cargo.toml"});
        let p = ToolPreview::from_input("read_file", &input);
        match p {
            ToolPreview::FileRead { path } => assert_eq!(path, "Cargo.toml"),
            _ => panic!("expected FileRead variant"),
        }
    }

    #[test]
    fn tool_preview_web_fetch_captures_url_method() {
        let input = serde_json::json!({"url": "https://api.example.com", "method": "POST"});
        let p = ToolPreview::from_input("web_fetch", &input);
        match p {
            ToolPreview::WebFetch { url, method } => {
                assert_eq!(url, "https://api.example.com");
                assert_eq!(method, "POST");
            }
            _ => panic!("expected WebFetch variant"),
        }
    }

    #[test]
    fn tool_preview_generic_for_unknown_tool() {
        let input = serde_json::json!({"foo": "bar", "n": 42});
        let p = ToolPreview::from_input("some_unknown_tool", &input);
        matches!(p, ToolPreview::Generic { .. });
    }

    #[test]
    fn tool_preview_to_plain_lines_backward_compat() {
        let input = serde_json::json!({"command": "ls -la", "working_dir": "."});
        let p = ToolPreview::from_input("bash_exec", &input);
        let lines = p.to_plain_lines();
        assert!(lines.iter().any(|l| l.contains("ls -la")));
        assert!(lines.iter().any(|l| l.contains('.')));
    }

    #[test]
    fn tool_preview_content_height_is_positive() {
        for (name, input) in [
            ("bash_exec", serde_json::json!({"command": "ls"})),
            (
                "edit_file",
                serde_json::json!({"path": "a.rs", "old_content": "", "new_content": ""}),
            ),
            (
                "write_file",
                serde_json::json!({"path": "b.rs", "content": "x"}),
            ),
            ("read_file", serde_json::json!({"path": "c.rs"})),
            (
                "web_fetch",
                serde_json::json!({"url": "http://x", "method": "GET"}),
            ),
            ("unknown", serde_json::json!({"k": "v"})),
        ] {
            let p = ToolPreview::from_input(name, &input);
            assert!(p.content_height() > 0, "height should be > 0 for {name}");
        }
    }

    // ── ApprovalContext + from_tool_call_with_context ──────────────────────────

    #[test]
    fn approval_context_new_file_medium_risk() {
        let ctx = ApprovalContext {
            is_new_file: true,
            agent_id: "a1".into(),
        };
        let input = serde_json::json!({"path": "new_module.rs", "content": ""});
        let r = RiskLevel::from_tool_call_with_context("write_file", &input, &ctx);
        // New non-critical file → Medium
        assert_eq!(r, RiskLevel::Medium);
    }

    #[test]
    fn approval_context_critical_file_high_risk() {
        let ctx = ApprovalContext::default();
        let input = serde_json::json!({"path": "Cargo.toml", "content": ""});
        let r = RiskLevel::from_tool_call_with_context("write_file", &input, &ctx);
        assert_eq!(r, RiskLevel::High);
    }

    // ── is_new_file helper ────────────────────────────────────────────────────

    #[test]
    fn is_new_file_with_explicit_false() {
        let input = serde_json::json!({"path": "x.rs", "existing": false});
        assert!(is_new_file(&input));
    }

    #[test]
    fn is_new_file_with_explicit_true() {
        let input = serde_json::json!({"path": "x.rs", "existing": true});
        assert!(!is_new_file(&input));
    }

    #[test]
    fn is_new_file_absent_field_returns_false() {
        let input = serde_json::json!({"path": "x.rs"});
        assert!(!is_new_file(&input));
    }
}
