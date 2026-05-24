# TUI Architecture

## Why Rust + Ratatui for Agent TUIs

Building a production-grade AI orchestration terminal in Rust with Ratatui is not a stylistic preference — it is the correct engineering choice for the following technical reasons:

### 1. Fearless Concurrency in the Rendering Pipeline

The `xaft` TUI must simultaneously render output from multiple agents, shell commands, and event streams without data races. In Python or Node.js, this requires careful GIL coordination or callback hell. In Rust, the `Arc<Mutex<AppState>>` pattern combined with Tokio tasks provides compile-time proof that the shared TUI state cannot be corrupted.

### 2. Deterministic Frame Timing

LLM tokens arrive at 50–500ms intervals. Shell commands produce bursts of output. Ratatui's immediate-mode rendering model redraws the full terminal state 30 times per second regardless of event rate — no partial frame artifacts, no flickering from deferred updates. The frame budget is ~33ms. Rust's lack of GC means frame timing is deterministic.

### 3. Byte-Level Terminal Control

Ratatui uses crossterm to operate at the raw terminal byte level. This enables:
- Custom Unicode rendering for diff syntax highlighting
- Precise cursor positioning for streaming text
- Mouse support for click-to-approve dialogs
- Alternate screen buffer (no scroll history contamination)
- 256-color and true-color rendering of code and diffs

### 4. Zero-Cost Abstraction for Rendering Logic

Ratatui widgets are Rust structs implementing `Widget`. The borrow checker ensures that widget state referenced in `render()` is not mutated while drawing. This is impossible to achieve safely in a dynamic language.

### 5. Streaming Output Integration

`xaft` connects agent `StreamEvent` streams directly to the TUI via `mpsc` channels. Ratatui's model allows the render loop to drain the entire event queue in a single frame tick, rendering all accumulated text deltas as one draw call — more efficient than per-token redraws.

---

## TUI Application Architecture

```
┌────────────────── TuiApp ─────────────────────────────────────┐
│                                                                 │
│  ┌──────────────┐    ┌──────────────────────────────────────┐  │
│  │ AppState     │    │ Event Loop (async task)              │  │
│  │              │    │                                      │  │
│  │ session      │    │  tokio::select! {                    │  │
│  │ plan_steps   │◄───│    event = ui_rx.recv() => {         │  │
│  │ agent_panes  │    │      update_state(&mut state, event) │  │
│  │ cost_metrics │    │    }                                 │  │
│  │ approval_dlg │    │    _ = tick.tick() => {              │  │
│  └──────────────┘    │      terminal.draw(render(&state))   │  │
│                       │    }                                 │  │
│                       │  }                                   │  │
│                       └──────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Core Types

```rust
pub struct TuiApp {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: Arc<Mutex<AppState>>,
    ui_tx: mpsc::Sender<UiEvent>,
    ui_rx: mpsc::Receiver<UiEvent>,
}

impl TuiApp {
    pub async fn run(mut self) -> Result<(), XaftError> {
        let mut tick = tokio::time::interval(Duration::from_millis(33));

        loop {
            tokio::select! {
                // Drain all pending UI events
                Some(event) = self.ui_rx.recv() => {
                    let mut state = self.state.lock().await;
                    let action = state.handle_event(event);
                    drop(state);  // release lock before draw

                    match action {
                        Action::Quit => break,
                        Action::RequestApproval(decision) => {
                            // Forward approval decision to orchestrator
                            self.approval_gate.respond(decision, None).await;
                        }
                        Action::None => {}
                    }
                }
                // Render frame on tick
                _ = tick.tick() => {
                    let state = self.state.lock().await;
                    self.terminal.draw(|frame| render(frame, &state))?;
                }
            }
        }

        restore_terminal(self.terminal)
    }
}
```

## Layout System

```
┌─────────────────────────────────────────────────────────────────┐
│ [Tab: Output] [Tab: Plan] [Tab: Diff] [Tab: Shell] [Tab: Logs] │  ← tabs
├────────────────────────────────┬────────────────────────────────┤
│                                │                                 │
│   Left Pane                    │   Right Pane                   │
│                                │                                 │
│   [Plan Tree / Agent List]     │   [Agent Output / Diff /       │
│   Step 1: ✓ Index files        │    Shell Console / Log]        │
│   Step 2: ⟳ Edit auth.rs       │                                │
│   Step 3: · Run tests          │   < streaming text here >      │
│   Step 4: · Commit             │                                │
│                                │                                │
├────────────────────────────────┴────────────────────────────────┤
│  Tool: write_file src/auth.rs · Turn 3/20 · $0.023 · 1,240 tk  │  ← status bar
└─────────────────────────────────────────────────────────────────┘
```

### Layout Implementation

```rust
pub fn render(frame: &mut Frame, state: &AppState) {
    // Outer layout: tabs + content + status bar
    let outer = Layout::vertical([
        Constraint::Length(1),  // tab bar
        Constraint::Min(0),     // content
        Constraint::Length(1),  // status bar
    ]).split(frame.area());

    render_tab_bar(frame, state, outer[0]);

    // Content: left/right split
    let content = Layout::horizontal([
        Constraint::Percentage(30),
        Constraint::Percentage(70),
    ]).split(outer[1]);

    render_left_pane(frame, state, content[0]);
    render_right_pane(frame, state, content[1]);

    render_status_bar(frame, state, outer[2]);

    // Modal: approval dialog (rendered on top)
    if let Some(ref approval) = state.pending_approval {
        render_approval_dialog(frame, approval, frame.area());
    }
}
```

## Pane System

### AgentOutputPane

The primary text streaming pane. Renders incremental token output from the active agent.

```rust
pub struct AgentOutputPane {
    pub lines: VecDeque<StyledLine>,  // circular buffer, max 2000 lines
    pub scroll_offset: usize,
    pub auto_scroll: bool,            // follows bottom unless user scrolls up
    pub agent_name: String,
    pub current_tool: Option<String>,
}

pub fn render_agent_output(frame: &mut Frame, pane: &AgentOutputPane, area: Rect) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let start = if pane.auto_scroll {
        pane.lines.len().saturating_sub(visible_height)
    } else {
        pane.scroll_offset
    };

    let visible: Vec<Line> = pane.lines.iter()
        .skip(start)
        .take(visible_height)
        .map(|sl| sl.to_ratatui_line())
        .collect();

    let title = format!(
        " {} {} ",
        pane.agent_name,
        pane.current_tool.as_deref().map(|t| format!("[{t}]")).unwrap_or_default()
    );

    let paragraph = Paragraph::new(visible)
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);

    // Scrollbar
    let mut scroll_state = ScrollbarState::new(pane.lines.len())
        .position(start);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        area,
        &mut scroll_state,
    );
}
```

### PlanTreePane

Visualizes the plan as a tree with real-time step status.

```rust
pub struct PlanStepUi {
    pub id: String,
    pub description: String,
    pub status: StepStatus,
    pub agent_name: String,
    pub duration_ms: Option<f64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone)]
pub enum StepStatus {
    Pending,
    Running { started_at: Instant },
    Complete { duration_ms: f64 },
    Failed { reason: String },
    Skipped,
}

pub fn render_plan_tree(frame: &mut Frame, steps: &[PlanStepUi], current: Option<usize>, area: Rect) {
    let items: Vec<ListItem> = steps.iter().enumerate().map(|(i, step)| {
        let (icon, color) = match &step.status {
            StepStatus::Pending => ("○", Color::DarkGray),
            StepStatus::Running { started_at } => {
                let spinner = ["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];
                let idx = (started_at.elapsed().as_millis() / 100) as usize % spinner.len();
                (spinner[idx], Color::Yellow)
            }
            StepStatus::Complete { .. } => ("✓", Color::Green),
            StepStatus::Failed { .. } => ("✗", Color::Red),
            StepStatus::Skipped => ("─", Color::DarkGray),
        };

        let is_current = current == Some(i);
        let style = Style::default().fg(color)
            .add_modifier(if is_current { Modifier::BOLD } else { Modifier::empty() });

        let duration = step.duration_ms.map(|d| format!(" {:.0}ms", d)).unwrap_or_default();
        let text = format!("{icon} {}{duration}", step.description);

        ListItem::new(text).style(style)
    }).collect();

    frame.render_widget(
        List::new(items).block(Block::bordered().title(" Plan ")),
        area,
    );
}
```

## Keyboard Routing

```rust
pub enum KeyBinding {
    Tab,           // cycle active pane
    ShiftTab,      // cycle backwards
    Char('q'),     // quit (with confirmation)
    Char('s'),     // suspend current task
    Char('r'),     // resume suspended task
    Char('a'),     // approve pending tool call
    Char('d'),     // deny pending tool call
    Char('c'),     // copy current pane content
    Char('?'),     // show help overlay
    Up | Down,     // scroll current pane
    PageUp | PageDown,  // scroll by page
    Char('g'),     // scroll to top
    Char('G'),     // scroll to bottom (re-enable auto-scroll)
    Char('1'..='5'), // switch tab by number
}

pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('a') if state.pending_approval.is_some() => Action::RequestApproval(true),
        KeyCode::Char('d') if state.pending_approval.is_some() => Action::RequestApproval(false),
        KeyCode::Tab => { state.cycle_pane(); Action::None }
        KeyCode::Up => { state.scroll_up(3); Action::None }
        KeyCode::Down => { state.scroll_down(3); Action::None }
        _ => Action::None,
    }
}
```

## Performance Requirements

| Metric | Target | Mechanism |
|---|---|---|
| Frame rate | 30fps steady | 33ms tick interval |
| Frame budget | ≤ 25ms render time | Profiled with `criterion` |
| Text buffer | ≤ 2000 lines/pane | `VecDeque` with `pop_front` |
| Event queue | ≤ 1024 pending events | Bounded `mpsc` |
| Lock hold time | ≤ 1ms | Clone data before await |
| Terminal resize | ≤ 1 frame lag | `crossterm::event::Event::Resize` |

## Terminal Setup and Teardown

```rust
pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, XaftError> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

pub fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<(), XaftError> {
    crossterm::terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

// Always restore on panic
pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crossterm::terminal::disable_raw_mode().ok();
        execute!(std::io::stdout(), LeaveAlternateScreen).ok();
        original(info);
    }));
}
```

## References

- Ratatui: https://ratatui.rs/
- agtrs: `agtrs-runtime/src/streaming.rs` (StreamEvent)
- Next: [Layout Engine →](02_layout_engine.md)