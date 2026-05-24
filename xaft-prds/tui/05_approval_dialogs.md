# Approval Dialogs

## Approval Modal

When a high-risk tool call is intercepted, execution pauses and a modal dialog appears:

```
┌──────────────── ⚠ Approval Required ──────────────────────┐
│                                                             │
│  Agent: code_agent  ·  Risk: HIGH                          │
│                                                             │
│  Tool: run_command                                          │
│  Input:                                                     │
│  {                                                          │
│    "command": "rm -rf ./build",                            │
│    "working_dir": "/project"                               │
│  }                                                          │
│                                                             │
│  Preview:                                                   │
│  This will delete the build/ directory (2.1 GB)            │
│                                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────────────┐ │
│  │ [a] Approve │  │ [d] Deny  │  │ [e] Edit command       │ │
│  └──────────┘  └──────────┘  └──────────────────────────┘ │
│                                                             │
│  Auto-deny in: 25s ████████████████░░░░░░░░░░              │
└─────────────────────────────────────────────────────────────┘
```

## Approval Dialog Implementation

```rust
pub struct ApprovalDialogState {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub risk_level: RiskLevel,
    pub preview: String,          // human-readable impact description
    pub deadline: Instant,
    pub timeout_secs: u64,
}

pub fn render_approval_dialog(frame: &mut Frame, state: &ApprovalDialogState, area: Rect) {
    // Center a popup over the main content
    let popup_area = center_rect(area, 70, 50);

    // Clear background
    frame.render_widget(Clear, popup_area);

    let block = Block::bordered()
        .title(format!(" ⚠ Approval Required · {} ", state.risk_level))
        .border_style(Style::default().fg(Color::Red))
        .title_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Content layout
    let sections = Layout::vertical([
        Constraint::Length(2),  // agent + risk
        Constraint::Length(8),  // tool + input
        Constraint::Length(3),  // preview
        Constraint::Length(3),  // buttons
        Constraint::Length(2),  // countdown
    ]).split(inner);

    // Countdown gauge
    let elapsed = state.deadline.saturating_duration_since(Instant::now());
    let remaining_pct = elapsed.as_secs_f64() / state.timeout_secs as f64;
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(Color::Yellow))
        .ratio(remaining_pct)
        .label(format!("Auto-deny in: {}s", elapsed.as_secs()));
    frame.render_widget(gauge, sections[4]);
}
```

## Auto-Approval Configuration

```toml
# .xaft/config.toml
[safety.auto_approve]
enabled = false
risk_threshold = "medium"  # auto-approve Low and Medium, pause on High

# Specific tool allowlisting
[[safety.auto_approve.tools]]
name = "write_file"
paths = ["src/**", "tests/**"]  # only auto-approve writes in these paths
risk = "medium"

[[safety.auto_approve.tools]]
name = "run_cargo"
subcommands = ["check", "test", "clippy"]  # only these subcommands
risk = "medium"
```

## Approval Audit

Every approval decision is written to the audit log:

```json
{
  "ts": "2026-01-15T10:23:45Z",
  "event": "approval_decision",
  "tool_name": "run_command",
  "input": {"command": "rm -rf ./build"},
  "risk_level": "High",
  "approved": false,
  "reason": "user rejected",
  "session_id": "ses-abc123",
  "task_id": "tsk-def456"
}
```

## References

- agtrs: `agtrs-runtime/src/task.rs` (RiskLevel)
- agtrs guide: `guides/13-approval-gates.md`
- Next: [Dashboards →](06_dashboards.md)
