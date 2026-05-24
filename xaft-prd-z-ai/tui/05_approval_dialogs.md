# XAFT Approval Dialogs

## When Tools Require Confirmation

The agtrs framework supports `requires_confirmation` on tool definitions. When a tool
call triggers confirmation, the agent runtime pauses and emits an `ApprovalRequired`
signal. The xaft TUI must render this as a blocking dialog that:

1. **Shows exactly what the tool wants to do** (command, file edit, network request)
2. **Indicates risk level** (LOW / MEDIUM / HIGH / CRITICAL)
3. **Provides keyboard shortcuts** for accept, reject, and edit
4. **Blocks the agent** without freezing the TUI
5. **Supports batch approval** for repeated similar operations

## Approval Dialog Rendering

### Visual Design

```
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│  ⚠ APPROVAL REQUIRED — FileEditor                           │
│                                                              │
│  File:     src/auth/token.rs                                 │
│  Operation: Edit (2 hunks, 4 lines changed)                 │
│  Agent:    coordinator-01                                    │
│  Risk:     ████░░░░  MEDIUM                                 │
│                                                              │
│  ┌─ Hunk 1 ──────────────────────────────────────────────┐  │
│  │ 15 │-        .duration_since(UNIX_EPOCH)?.as_secs();  │  │
│  │ 15 │+        .duration_since(UNIX_EPOCH)?             │  │
│  │ 16 │+        .as_secs();                              │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                              │
│  ┌─ Hunk 2 ──────────────────────────────────────────────┐  │
│  │ 18 │-    if token.expiry < now {                      │  │
│  │ 18 │+    if token.expiry <= now {                     │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                              │
│  [a] Approve  [r] Reject  [e] Edit  [v] View full file     │
│  [A] Approve all for this agent  [s] Skip for now           │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

### Overlay Positioning

The approval dialog is rendered as a floating overlay on top of the existing layout.
It is centered in the terminal and sized to fit the content:

```rust
/// Calculate overlay rect: centered, with padding
pub fn approval_overlay_rect(terminal_size: (u16, u16), content_height: u16) -> Rect {
    let width = (terminal_size.0.saturating_sub(4)).min(80).max(50);
    let height = content_height.min(terminal_size.1.saturating_sub(4)).max(10);
    let x = (terminal_size.0.saturating_sub(width)) / 2;
    let y = (terminal_size.1.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
```

## Tool Input Preview

### Truncated JSON Preview

For tools with structured JSON input, xaft renders a truncated, syntax-highlighted preview:

```
┌──────────────────────────────────────────────────────────────┐
│  ⚠ APPROVAL REQUIRED — Bash                                  │
│                                                              │
│  Command:                                                    │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ cargo test --lib auth::token -- --nocapture    ││
│  └─────────────────────────────────────────────────────────┘│
│                                                              │
│  Working Dir: /home/user/project                             │
│  Timeout:     30s                                            │
│  Risk:        ██░░░░░░  LOW                                  │
│                                                              │
│  [a] Approve  [r] Reject  [e] Edit command  [s] Skip        │
└──────────────────────────────────────────────────────────────┘
```

### File Path Preview

```
┌──────────────────────────────────────────────────────────────┐
│  ⚠ APPROVAL REQUIRED — WriteFile                             │
│                                                              │
│  File:     src/auth/new_module.rs                            │
│  Size:     2,450 bytes (new file)                            │
│  Agent:    file-creator-03                                   │
│  Risk:     ███░░░░░  MEDIUM (new file creation)              │
│                                                              │
│  Preview (first 15 lines):                                   │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  1 │ use crate::auth::token::Token;                     ││
│  │  2 │ use crate::auth::error::AuthError;                 ││
│  │  3 │                                                      ││
│  │  4 │ /// Validates refresh tokens against the            ││
│  │  5 │ /// configured secret key.                          ││
│  │  6 │ pub fn validate_refresh(                            ││
│  │  7 │     token: &str,                                    ││
│  │  8 │ ) -> Result<Token, AuthError> {                     ││
│  │  9 │     ...                                             ││
│  │    │ (15 more lines — [v] to view full file)             ││
│  └─────────────────────────────────────────────────────────┘│
│                                                              │
│  [a] Approve  [r] Reject  [e] Edit  [v] View full file     │
└──────────────────────────────────────────────────────────────┘
```

### Tool-Specific Preview Rendering

```rust
/// Render tool input preview based on tool type
pub fn render_tool_preview(
    tool_name: &str,
    input: &serde_json::Value,
    area: Rect,
    buf: &mut Buffer,
) {
    match tool_name {
        "Bash" => render_bash_preview(input, area, buf),
        "FileEditor" => render_file_editor_preview(input, area, buf),
        "WriteFile" => render_write_file_preview(input, area, buf),
        "ReadFile" => render_read_file_preview(input, area, buf),
        "WebFetch" => render_web_fetch_preview(input, area, buf),
        _ => render_generic_preview(input, area, buf),
    }
}

fn render_bash_preview(input: &serde_json::Value, area: Rect, buf: &mut Buffer) {
    let command = input.get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");

    let workdir = input.get("working_dir")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let timeout = input.get("timeout_secs")
        .and_then(|v| v.as_u64())
        .map(|t| format!("{}s", t))
        .unwrap_or_else(|| "none".into());

    // Render command in a highlighted box
    let label = Span::styled(" Command:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    let cmd = Span::styled(format!(" {}", command), Style::default().fg(Color::White));
    Line::from(vec![label, cmd]).render(area, buf);

    // Render working directory and timeout
    let dir_label = Span::styled(" Working Dir: ", Style::default().fg(Color::DarkGray));
    let dir_val = Span::styled(workdir, Style::default().fg(Color::Gray));
    Line::from(vec![dir_label, dir_val]).render(
        Rect::new(area.x, area.y + 1, area.width, 1), buf
    );

    let timeout_label = Span::styled(" Timeout: ", Style::default().fg(Color::DarkGray));
    let timeout_val = Span::styled(timeout, Style::default().fg(Color::Gray));
    Line::from(vec![timeout_label, timeout_val]).render(
        Rect::new(area.x, area.y + 2, area.width, 1), buf
    );
}

fn render_generic_preview(input: &serde_json::Value, area: Rect, buf: &mut Buffer) {
    // Truncate JSON to fit area
    let json = serde_json::to_string_pretty(input).unwrap_or_default();
    let max_chars = (area.width as usize - 4) * (area.height as usize - 2);
    let display = if json.len() > max_chars {
        format!("{}…", &json[..max_chars.saturating_sub(1)])
    } else {
        json
    };

    // Render with JSON syntax highlighting
    for (i, line) in display.lines().take(area.height as usize).enumerate() {
        let spans = highlight_json_line(line);
        Line::from(spans).render(
            Rect::new(area.x + 2, area.y + i as u16, area.width - 4, 1),
            buf,
        );
    }
}
```

## Risk Level Display

### Risk Level Computation

Risk levels are derived from the `TaskState` approval request and the tool's own metadata:

```rust
/// Risk level for approval requests
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Low,      // Read-only operations, non-destructive queries
    Medium,   // File edits, command execution with known behavior
    High,     // File deletion, command execution with side effects
    Critical, // Destructive operations, irreversible changes
}

impl RiskLevel {
    /// Determine risk level from tool call context
    pub fn from_tool_call(tool_name: &str, input: &serde_json::Value, context: &ApprovalContext) -> Self {
        match tool_name {
            // Read-only tools: always LOW
            "ReadFile" | "ListFiles" | "Grep" | "Glob" => RiskLevel::Low,

            // Bash: risk depends on the command
            "Bash" => {
                let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if cmd.starts_with("rm ") || cmd.starts_with("rm -") {
                    RiskLevel::Critical
                } else if cmd.contains("sudo") || cmd.contains("chmod") || cmd.contains("chown") {
                    RiskLevel::High
                } else if cmd.starts_with("cargo ") || cmd.starts_with("npm ") || cmd.starts_with("git ") {
                    RiskLevel::Low
                } else {
                    RiskLevel::Medium
                }
            },

            // File editing: medium by default, high if modifying critical files
            "FileEditor" | "WriteFile" => {
                let path = input.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                if is_critical_file(path) {
                    RiskLevel::High
                } else if is_new_file(input) {
                    RiskLevel::Medium // New file creation
                } else {
                    RiskLevel::Medium
                }
            },

            _ => RiskLevel::Medium, // Unknown tools default to medium
        }
    }

    /// Visual style for risk level
    pub fn style(&self) -> Style {
        match self {
            RiskLevel::Low      => Style::default().fg(Color::Green),
            RiskLevel::Medium   => Style::default().fg(Color::Yellow),
            RiskLevel::High     => Style::default().fg(Color::Red),
            RiskLevel::Critical => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }

    /// Risk gauge fill fraction
    pub fn gauge_fraction(&self) -> f64 {
        match self {
            RiskLevel::Low      => 0.25,
            RiskLevel::Medium   => 0.50,
            RiskLevel::High     => 0.75,
            RiskLevel::Critical => 1.00,
        }
    }
}

/// Critical file patterns that elevate risk
fn is_critical_file(path: &str) -> bool {
    let critical_patterns = [
        "Cargo.toml", "package.json", "go.mod",
        ".env", ".gitignore", "Dockerfile",
        "main.rs", "main.go", "index.ts", "index.js",
        "config", "secrets", "credentials",
    ];
    critical_patterns.iter().any(|p| path.contains(p))
}
```

### Risk Gauge Widget

```rust
/// Risk level gauge display
pub fn render_risk_gauge(risk: RiskLevel, area: Rect, buf: &mut Buffer) {
    let (label, color, fraction) = match risk {
        RiskLevel::Low      => ("LOW", Color::Green, 0.25),
        RiskLevel::Medium   => ("MEDIUM", Color::Yellow, 0.50),
        RiskLevel::High     => ("HIGH", Color::Red, 0.75),
        RiskLevel::Critical => ("CRITICAL", Color::Red, 1.00),
    };

    // Label
    let label_span = Span::styled(
        format!(" Risk: "),
        Style::default().fg(Color::Gray),
    );

    // Gauge
    let gauge_width = 12u16;
    let filled = (gauge_width as f64 * fraction).round() as u16;
    let empty = gauge_width - filled;

    let filled_span = Span::styled(
        "█".repeat(filled as usize),
        Style::default().fg(color),
    );
    let empty_span = Span::styled(
        "░".repeat(empty as usize),
        Style::default().fg(Color::DarkGray),
    );
    let level_span = Span::styled(
        format!(" {}", label),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );

    Line::from(vec![label_span, filled_span, empty_span, level_span])
        .render(area, buf);
}
```

## Keyboard Shortcuts

### Accept/Reject/Edit Shortcuts

```
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│  ┌─────────┐  ┌─────────┐  ┌──────────────┐  ┌──────────┐ │
│  │  [a]     │  │  [r]     │  │  [e]          │  │  [v]      │ │
│  │ Approve  │  │ Reject  │  │ Edit in $EDITOR│  │ View file │ │
│  └─────────┘  └─────────┘  └──────────────┘  └──────────┘ │
│                                                              │
│  ┌────────────────────┐  ┌─────────────────┐               │
│  │  [A]                │  │  [s]             │               │
│  │ Approve all for     │  │ Skip (decide     │               │
│  │ this agent session  │  │ later)           │               │
│  └────────────────────┘  └─────────────────┘               │
│                                                              │
│  ┌──────────────────────────┐                               │
│  │  [R] Reject all pending  │                               │
│  └──────────────────────────┘                               │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

```rust
/// Handle approval key events
pub fn handle_approval_key(
    state: &mut AppState,
    key: KeyEvent,
) -> Result<ApprovalAction> {
    let action = match key.code {
        KeyCode::Char('a') => ApprovalAction::Approve,
        KeyCode::Char('r') => ApprovalAction::Reject,
        KeyCode::Char('e') => ApprovalAction::Edit,
        KeyCode::Char('v') => ApprovalAction::ViewFull,
        KeyCode::Char('A') => ApprovalAction::ApproveAll,
        KeyCode::Char('R') => ApprovalAction::RejectAll,
        KeyCode::Char('s') => ApprovalAction::Skip,
        KeyCode::Esc       => ApprovalAction::Skip,
        _ => return Ok(ApprovalAction::None),
    };

    Ok(action)
}
```

## Blocking Without Freezing

### The Problem

When the agent requires approval, it blocks on a `oneshot::Sender<ApprovalResult>`.
If the TUI also blocks waiting for user input, we get a deadlock: the TUI can't
render (it's waiting) and the agent can't proceed (it's waiting).

### The Solution: Async Approval Channels

```rust
/// Approval system: agent blocks on a oneshot channel, TUI resolves it
pub struct ApprovalQueue {
    /// Pending approval requests
    pending: VecDeque<PendingApproval>,

    /// Currently displayed approval (top of queue)
    current: Option<PendingApproval>,

    /// Approval history for this session
    history: Vec<ApprovalRecord>,
}

pub struct PendingApproval {
    /// Unique ID for this approval request
    pub id: ApprovalId,

    /// Tool name
    pub tool_name: String,

    /// Tool input (JSON)
    pub tool_input: serde_json::Value,

    /// Risk level
    pub risk: RiskLevel,

    /// Agent that made the request
    pub agent_id: AgentId,

    /// Timestamp of the request
    pub requested_at: chrono::DateTime<chrono::Utc>,

    /// Sender to notify the agent runtime of the decision
    pub responder: oneshot::Sender<ApprovalResult>,

    /// Whether the user has already pre-approved this type
    pub pre_approved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalResult {
    Approved,
    Rejected,
    Edited { modified_input: serde_json::Value },
    Skipped,
}

impl ApprovalQueue {
    /// Called by the agent runtime: enqueue a new approval request
    pub fn enqueue(
        &mut self,
        tool_name: String,
        tool_input: serde_json::Value,
        risk: RiskLevel,
        agent_id: AgentId,
    ) -> oneshot::Receiver<ApprovalResult> {
        let (tx, rx) = oneshot::channel();
        let approval = PendingApproval {
            id: ApprovalId::new(),
            tool_name,
            tool_input,
            risk,
            agent_id,
            requested_at: chrono::Utc::now(),
            responder: tx,
            pre_approved: false,
        };

        self.pending.push_back(approval);
        self.current = self.pending.front().cloned();

        rx
    }

    /// Called by the TUI: resolve the current approval
    pub fn resolve_current(&mut self, result: ApprovalResult) -> Option<()> {
        let approval = self.pending.pop_front()?;

        // Record in history
        self.history.push(ApprovalRecord {
            id: approval.id,
            tool_name: approval.tool_name.clone(),
            risk: approval.risk,
            result: result.clone(),
            decided_at: chrono::Utc::now(),
        });

        // Notify the agent runtime
        let _ = approval.responder.send(result);

        // Update current to next pending
        self.current = self.pending.front().cloned();

        Some(())
    }
}
```

### TUI Event Loop with Approval

```rust
/// The TUI event loop never blocks. Approval waiting happens via async channels.
async fn run_with_approvals(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: Arc<RwLock<AppState>>,
    mut channels: TuiChannels,
) -> Result<()> {
    loop {
        // Process all events non-blocking
        tokio::select! {
            // Terminal events (always responsive)
            Some(event) = channels.term_events.recv() => {
                let mut s = state.write().await;
                match event {
                    TermEvent::Key(key) => {
                        // If approval is pending, route key to approval handler
                        if s.approval_queue.has_pending() {
                            let action = handle_approval_key(&mut s, key)?;
                            if action != ApprovalAction::None {
                                execute_approval_action(&mut s, action);
                            }
                        } else {
                            s.handle_key_event(key)?;
                        }
                    }
                    TermEvent::Resize(w, h) => { /* ... */ }
                    _ => {}
                }
            }

            // Agent signals (token stream, tool calls, etc.)
            Some(signal) = channels.agent_events.recv() => {
                let mut s = state.write().await;
                s.handle_agent_signal(signal);
            }

            // Render tick
            _ = channels.render_tick.tick() => {
                let s = state.read().await;
                if s.is_dirty() {
                    terminal.draw(|frame| render_frame(frame, &s))?;
                }
            }
        }

        if state.read().await.should_quit() { break; }
    }

    Ok(())
}
```

## Batch Approval Mode

### Auto-Approve Rules

Users can configure auto-approve rules for specific tool/agent combinations:

```rust
/// Auto-approve configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoApproveConfig {
    /// Rules for auto-approval
    pub rules: Vec<AutoApproveRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoApproveRule {
    /// Tool name pattern (supports wildcards)
    pub tool: String,

    /// Agent ID pattern (supports wildcards)
    pub agent: String,

    /// Maximum risk level to auto-approve
    pub max_risk: RiskLevel,

    /// Optional: specific file path pattern
    pub path_pattern: Option<String>,

    /// Optional: specific command pattern (for Bash)
    pub command_pattern: Option<String>,

    /// Maximum number of auto-approvals per rule per session
    pub max_approvals: Option<u32>,

    /// Current approval count
    #[serde(skip)]
    pub approval_count: u32,
}
```

### Example Configuration

```toml
# ~/.config/xaft/auto-approve.toml

[[rules]]
tool = "ReadFile"
max_risk = "Low"
max_approvals = 100

[[rules]]
tool = "Bash"
command_pattern = "cargo test*"
max_risk = "Low"
max_approvals = 20

[[rules]]
tool = "Bash"
command_pattern = "git diff*"
max_risk = "Low"
max_approvals = 10

[[rules]]
tool = "FileEditor"
path_pattern = "src/**"
max_risk = "Medium"
max_approvals = 50
```

### Batch Approval at Runtime

When the user presses `A` (Approve All), xaft applies batch approval:

```rust
/// Batch approval: approve all pending requests for the same agent
pub fn approve_all_for_agent(state: &mut AppState, agent_id: &AgentId) {
    let to_approve: Vec<ApprovalId> = state.approval_queue.pending.iter()
        .filter(|a| &a.agent_id == agent_id)
        .filter(|a| a.risk <= RiskLevel::Medium) // Never auto-approve HIGH/CRITICAL
        .map(|a| a.id)
        .collect();

    for id in to_approve {
        state.approval_queue.resolve_by_id(id, ApprovalResult::Approved);
    }
}
```

### Batch Approval Visual

```
┌──────────────────────────────────────────────────────────────┐
│  ⚠ 3 PENDING APPROVALS                                      │
│                                                              │
│  1. FileEditor("src/auth/token.rs")    MEDIUM    [a] [r]    │
│  2. Bash("cargo test")                 LOW       [a] [r]    │
│  3. FileEditor("src/auth/mod.rs")      MEDIUM    [a] [r]    │
│                                                              │
│  [1/2/3] Approve individual  [A] Approve all  [R] Reject all│
│                                                              │
│  ⚡ Auto-approve: [p] ReadFile always  [o] cargo* commands   │
└──────────────────────────────────────────────────────────────┘
```

## Approval History

### Session Approval Log

```
┌──────────────────────────────────────────────────────────────────────┐
│  Approval History (12 decisions this session)                        │
│                                                                      │
│  #   │ Tool        │ Target                 │ Risk   │ Result │ Age  │
│ ─────┼─────────────┼────────────────────────┼────────┼────────┼──────│
│  1   │ ReadFile    │ src/main.rs            │ LOW    │ ✓ Auto │ 12m  │
│  2   │ Bash        │ cargo check            │ LOW    │ ✓ Auto │ 10m  │
│  3   │ FileEditor  │ src/auth/token.rs      │ MEDIUM │ ✓ User │  8m  │
│  4   │ Bash        │ cargo test             │ LOW    │ ✓ Auto │  7m  │
│  5   │ FileEditor  │ src/auth/mod.rs        │ MEDIUM │ ✓ User │  5m  │
│  6   │ Bash        │ rm -rf target/debug    │ HIGH   │ ✗ User │  4m  │
│  7   │ WriteFile   │ scripts/deploy.sh      │ HIGH   │ ✗ User │  3m  │
│  8   │ FileEditor  │ src/auth/token.rs      │ MEDIUM │ ✓ User │  2m  │
│  9   │ Bash        │ git diff               │ LOW    │ ✓ Auto │  1m  │
│ 10   │ FileEditor  │ src/lib.rs             │ MEDIUM │ ✓ User │ 30s  │
│ 11   │ ReadFile    │ Cargo.toml             │ LOW    │ ✓ Auto │ 15s  │
│ 12   │ Bash        │ cargo clippy           │ LOW    │ ○ Pend │ now  │
│                                                                      │
│  Summary: 9 approved (5 auto, 4 manual), 2 rejected, 1 pending      │
│  Auto-approve savings: 5 dialogs skipped                             │
│                                                                      │
│  [Esc] Close  [u] Undo last approval  [c] Clear history             │
└──────────────────────────────────────────────────────────────────────┘
```

```rust
/// Approval history record
#[derive(Debug, Clone)]
pub struct ApprovalRecord {
    pub id: ApprovalId,
    pub tool_name: String,
    pub risk: RiskLevel,
    pub result: ApprovalResult,
    pub decided_at: chrono::DateTime<chrono::Utc>,
}

impl ApprovalQueue {
    /// Render approval history
    pub fn render_history(&self, area: Rect, buf: &mut Buffer) {
        // Header
        let header = Line::from(vec![
            Span::styled(" #  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tool       ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
            Span::styled("Risk  ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
            Span::styled("Result", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        ]);
        header.render(area, buf);

        // Rows
        for (i, record) in self.history.iter().enumerate() {
            let y = area.y + 1 + i as u16;
            if y >= area.bottom() { break; }

            let result_text = match &record.result {
                ApprovalResult::Approved => "✓",
                ApprovalResult::Rejected => "✗",
                ApprovalResult::Edited { .. } => "✎",
                ApprovalResult::Skipped => "○",
            };

            let result_color = match &record.result {
                ApprovalResult::Approved => Color::Green,
                ApprovalResult::Rejected => Color::Red,
                ApprovalResult::Edited { .. } => Color::Cyan,
                ApprovalResult::Skipped => Color::DarkGray,
            };

            let row = Line::from(vec![
                Span::styled(format!("{:>2} ", i + 1), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<12}", record.tool_name), Style::default().fg(Color::White)),
                Span::styled(format!("{:>6} ", record.risk.label()), record.risk.style()),
                Span::styled(result_text.to_string(), Style::default().fg(result_color)),
            ]);
            row.render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}
```

## Undo Approval

The `u` key in the approval history view undoes the most recent approval. This sends
a reversal signal to the agent runtime:

```rust
/// Undo the most recent approval
pub fn undo_last_approval(state: &mut AppState) -> Result<()> {
    if let Some(last) = state.approval_queue.history.last() {
        // The agent may have already acted on this approval.
        // We can only undo if the tool hasn't completed yet.
        if last.result == ApprovalResult::Approved {
            // Send cancellation signal to the agent
            state.cancel_agent_tool(last.agent_id, last.id)?;
        }
    }
    Ok(())
}
```

## Approval Flow State Machine

```
                    ┌──────────┐
                    │  Agent   │
                    │ requests │
                    │ approval │
                    └────┬─────┘
                         │
                         ▼
                 ┌───────────────┐
                 │  Check auto-  │
                 │  approve rules│
                 └──┬────────┬───┘
                    │        │
             Matches │        │ No match
                    ▼        │
            ┌──────────┐    │
            │ Auto-    │    │
            │ approve  │    │
            │ (no UI)  │    │
            └────┬─────┘    │
                 │           │
                 ▼           ▼
          ┌──────────────────────────┐
          │  Show Approval Dialog    │
          │  (overlay in TUI)        │
          │                          │
          │  [a]pprove  [r]eject     │
          │  [e]dit     [s]kip       │
          │  [A]ll      [R]eject all │
          └────────────┬─────────────┘
                       │
              ┌────────┼────────┐
              │        │        │
         Approve   Reject    Edit
              │        │        │
              ▼        ▼        ▼
     ┌──────────┐ ┌─────────┐ ┌──────────────┐
     │ Send     │ │ Send    │ │ Open $EDITOR │
     │ Approved │ │ Rejected│ │ with input   │
     │ to agent │ │ to agent│ │ → Modified   │
     └──────────┘ └─────────┘ │ input sent   │
                              └──────────────┘
```
