# Generate remaining files efficiently using heredocs
cat > ./02_layout_engine.md << 'EOF'
# Layout Engine

## Responsive Layout System

`xaft` TUI adapts to terminal dimensions. Minimum: 80×24. Recommended: 160×48.

```
Terminal width < 100:  Single-pane mode (no left/right split)
Terminal width ≥ 100:  Dual-pane mode (30% left, 70% right)
Terminal width ≥ 160:  Three-pane mode (20% left, 50% center, 30% right)
```

## Pane Registry

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pane {
    PlanTree,
    AgentOutput(usize),   // agent index
    DiffViewer,
    ShellConsole,
    CostDashboard,
    LogConsole,
}

pub struct PaneLayout {
    pub visible_panes: Vec<Pane>,
    pub focused_pane: Pane,
    pub sizes: HashMap<Pane, Constraint>,
}
```

## Tab System

Five tabs accessible via number keys (1–5) or Tab/Shift-Tab:

```
[1: Output] [2: Plan] [3: Diff] [4: Shell] [5: Logs]
```

- **Output**: Agent streaming text. Multiple agents → sub-tabs per agent.
- **Plan**: Plan step tree with status, timing, cost per step.
- **Diff**: Staged changes diff viewer. Sortable by file.
- **Shell**: Live shell command output. Scrollable history.
- **Logs**: Timestamped event log. Filterable by level/type.

## Adaptive Agent Panes

When multiple agents are active (parallel execution), the Output tab shows a split view:

```
[Agent: code_agent ⟳]  [Agent: review_agent ✓]
──────────────────────  ──────────────────────
< streaming output >    Last response:
                        No issues found.
```

## Status Bar Fields

```
[session_state] · [task_id-short] · [step N/M] · [tool_name] · [turns] · [$cost] · [tokens] · [elapsed]

Example:
Executing · task:ab12 · Step 2/5 · write_file · Turn 7/20 · $0.042 · 3,420 tk · 2m34s
```

## References

- Next: [Diff Viewer →](03_diff_viewer.md)
EOF

cat > ./03_diff_viewer.md << 'EOF'
# Diff Viewer

## Overview

The diff viewer pane renders unified diffs of agent-produced changes with syntax highlighting, navigation, and inline approval controls.

## Diff Acquisition

After each `write_file` or `apply_patch` tool call:
1. `WorkspaceEditor::diff(path, new_content)` produces a `UnifiedDiff`
2. A `PatchApplied` signal is emitted
3. TUI subscribes and updates `DiffViewerState`

## Rendering

```rust
pub struct DiffViewerState {
    pub files: Vec<FileDiff>,
    pub selected_file: usize,
    pub scroll_offset: usize,
    pub show_context_lines: usize,  // default 3
}

pub struct FileDiff {
    pub path: PathBuf,
    pub hunks: Vec<DiffHunk>,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub language: Option<String>,  // for syntax highlighting
}

pub fn render_diff(frame: &mut Frame, state: &DiffViewerState, area: Rect) {
    // File list on left, hunks on right
    let layout = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(75),
    ]).split(area);

    render_file_list(frame, &state.files, state.selected_file, layout[0]);
    render_hunks(frame, &state.files[state.selected_file].hunks, state.scroll_offset, layout[1]);
}

fn render_hunks(frame: &mut Frame, hunks: &[DiffHunk], scroll: usize, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for hunk in hunks {
        // Hunk header
        lines.push(Line::from(Span::styled(
            format!("@@ -{},{} +{},{} @@", hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count),
            Style::default().fg(Color::Cyan),
        )));

        for diff_line in &hunk.lines {
            let (prefix, color) = match diff_line.kind {
                DiffLineKind::Added   => ("+", Color::Green),
                DiffLineKind::Removed => ("-", Color::Red),
                DiffLineKind::Context => (" ", Color::DarkGray),
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(color)),
                Span::styled(&diff_line.content, Style::default().fg(color)),
            ]));
        }
    }

    let visible = &lines[scroll.min(lines.len().saturating_sub(1))..];
    frame.render_widget(
        Paragraph::new(visible.to_vec())
            .block(Block::bordered().title(" Changes ")),
        area,
    );
}
```

## Diff Navigation

| Key | Action |
|---|---|
| `j/k` or `↑/↓` | Scroll by line |
| `J/K` | Next/prev hunk |
| `n/p` | Next/prev file |
| `Enter` | Open file in `$EDITOR` |
| `y` | Accept all changes in file |
| `N` | Discard all changes in file |

## References

- agtrs: `agtrs-workspace/src/diff.rs`
- Next: [Streaming Panes →](04_streaming_panes.md)
EOF

cat > ./04_streaming_panes.md << 'EOF'
# Streaming Panes

## Token-Level Streaming Rendering

Each text delta from `StreamEvent::TextDelta` is appended to the active agent pane's line buffer. The render loop picks up accumulated deltas on the next 33ms tick.

```rust
fn handle_agent_text_delta(state: &mut AppState, agent_idx: usize, delta: String) {
    if let Some(pane) = state.agent_panes.get_mut(agent_idx) {
        // Append delta to current line
        if let Some(last) = pane.current_line.as_mut() {
            last.push_str(&delta);
            if delta.contains('\n') {
                // Commit line to buffer
                let line = pane.current_line.take().unwrap();
                if pane.lines.len() >= 2000 {
                    pane.lines.pop_front();
                }
                pane.lines.push_back(StyledLine::plain(line));
                pane.current_line = Some(String::new());
            }
        }
        if pane.auto_scroll {
            pane.scroll_offset = pane.lines.len().saturating_sub(1);
        }
    }
}
```

## Thinking Blocks

Extended thinking content (`StreamEvent::ThinkingDelta`) is rendered in a collapsed section, expandable with `t`:

```
[▶ Thinking...] (press 't' to expand)
```

When expanded:
```
[▼ Thinking]
I need to first understand the current auth structure.
Let me read auth.rs to see what's there before making changes...
```

## Tool Execution Feed

When a tool is executing, the agent pane shows a live status line:

```
▶ Calling: write_file("src/auth.rs")
  Bytes: 4,231 · Status: writing...
```

After completion:
```
✓ write_file("src/auth.rs") · 124ms · 4,231 bytes
```

## Shell Console Streaming

The Shell tab streams `cargo test` output in real time:

```rust
fn handle_shell_output(state: &mut AppState, command: &str, chunk: &str, is_stderr: bool) {
    let color = if is_stderr { Color::Yellow } else { Color::White };
    let styled = StyledLine { spans: vec![StyledSpan { text: chunk.to_string(), color }] };
    if state.shell_lines.len() >= 5000 {
        state.shell_lines.pop_front();
    }
    state.shell_lines.push_back(styled);
}
```

## References

- Next: [Approval Dialogs →](05_approval_dialogs.md)
EOF

cat > ./05_approval_dialogs.md << 'EOF'
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
EOF

cat > ./06_dashboards.md << 'EOF'
# Dashboards

## Cost Dashboard

The cost dashboard provides real-time financial visibility into agent activity.

```
┌─────────────────── Cost Dashboard ──────────────────────────┐
│                                                              │
│  Session Total: $0.142  ████████░░░░░░░░░░ Budget: $2.00   │
│  Task Total:    $0.089  ████░░░░░░░░░░░░░░                  │
│  Last Call:     $0.012                                       │
│                                                              │
│  Tokens Used:   12,450 in · 4,230 out · 0 cache_read        │
│                                                              │
│  Cost/Turn (sparkline)                                       │
│  ▁▂▃▄▃▂▁▃▄▅▄▃▂▁▂▃▄▅▆▅▄▃▂▃▄▅▆▇▆▅▄                         │
│                                                              │
│  Provider:      anthropic · claude-3-5-sonnet               │
│  Avg latency:   423ms/turn                                   │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

## Agent Activity Timeline

```rust
pub struct AgentActivityWidget {
    pub events: VecDeque<ActivityEvent>,  // last 100 events
    pub time_window: Duration,
}

pub enum ActivityEvent {
    ModelCall { started_at: Instant, duration: Duration, cost: f64 },
    ToolCall { started_at: Instant, duration: Duration, tool: String, success: bool },
    FileMod { path: PathBuf, bytes: usize },
}
```

Rendered as a horizontal timeline bar, color-coded by event type:
- Blue: LLM call
- Green: Successful tool
- Red: Failed tool  
- Yellow: File modification

## Token Window Gauge

```rust
fn render_token_gauge(frame: &mut Frame, used: usize, max: usize, area: Rect) {
    let pct = used as f64 / max as f64;
    let color = if pct > 0.9 { Color::Red }
                else if pct > 0.7 { Color::Yellow }
                else { Color::Green };

    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(color))
            .ratio(pct)
            .label(format!("{}/{} tokens ({:.0}%)", used, max, pct * 100.0))
            .block(Block::bordered().title(" Context Window ")),
        area,
    );
}
```

## References

- agtrs: `agtrs-runtime/src/signals.rs` (ModelCallComplete with cost_usd)
- Previous: [Approval Dialogs](05_approval_dialogs.md)
EOF

echo "TUI docs done"