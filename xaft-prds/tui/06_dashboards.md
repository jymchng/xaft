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
