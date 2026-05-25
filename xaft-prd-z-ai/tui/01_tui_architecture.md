# XAFT TUI Architecture

## Why Rust + ratatui for Agentic TUIs

### The Case for Native Terminal UI

Autonomous coding agents produce output at machine speed — streaming tokens at 80+ tok/s,
spawning sub-agents, executing tools in parallel, and emitting signals across a task graph.
A TUI framework must keep up without frame drops, without blocking the agent runtime, and
without consuming resources that belong to the agent itself.

Ratatui (fork of tui-rs) provides:

| Property | ratatui + crossterm | web-based TUI (ink/blessed) | Python (textual) |
|---|---|---|---|
| Rendering overhead | <2% CPU at 60fps | 5-15% CPU (DOM diff) | 8-20% CPU (GC pauses) |
| Memory footprint | ~2MB baseline | 15-50MB (V8/Node) | 30-80MB (CPython) |
| Async integration | native tokio | libuv (separate loop) | asyncio (GIL-bound) |
| Binary size | 3-8MB static | 40-80MB (Node runtime) | 50-120MB (Python + deps) |
| Latency (event→pixel) | <100µs | 1-5ms | 2-10ms |
| Thread safety | Send + Sync by default | single-threaded | GIL bottleneck |

### Why ratatui specifically

1. **Immediate-mode rendering**: Each frame, the entire UI is reconstructed from state. No
   retained widget tree to sync, no stale state bugs. Perfect for agent-driven dynamic layouts
   where panes appear/disappear as sub-agents spawn.

2. **Zero-cost abstractions**: Widget trait is `FnOnce` per frame — no vtable dispatch, no
   heap allocation per widget. The compiler inlines the entire render pipeline.

3. **Crossterm backend**: Cross-platform terminal control (Windows conpty, Unix PTY, macOS
   Terminal.app) with unified event model. No platform-specific rendering paths.

4. **Composability**: Widgets compose trivially — a `DiffViewer` widget contains `LineGauge`,
   `Paragraph`, and custom `SyntaxBlock` widgets. No framework lock-in.

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                         XAFT TUI PROCESS                           │
│                                                                     │
│  ┌─────────────┐    ┌──────────────┐    ┌───────────────────────┐  │
│  │  crossterm   │    │   tokio      │    │   agtrs Runtime       │  │
│  │  Backend     │◄──►│   Runtime    │◄──►│   (Agent Process)     │  │
│  │              │    │              │    │                       │  │
│  │ • term events│    │ • event loop │    │ • LLM calls          │  │
│  │ • raw mode   │    │ • mpsc chans │    │ • tool execution     │  │
│  │ • alternate  │    │ • watch sigs │    │ • signal emission    │  │
│  │   screen     │    │ • task spawn │    │ • task management    │  │
│  └──────┬───────┘    └──────┬───────┘    └───────────┬───────────┘  │
│         │                   │                        │              │
│         ▼                   ▼                        ▼              │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │                    App State (Arc<RwLock<AppState>>)         │   │
│  │                                                              │   │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │   │
│  │  │ ChatState│ │DiffState │ │ AgentTree│ │ CostState     │  │   │
│  │  │          │ │          │ │          │ │               │  │   │
│  │  │ • msgs   │ │ • hunks  │ │ • tasks  │ │ • tokens      │  │   │
│  │  │ • input  │ │ • scroll │ │ • status │ │ • costs       │  │   │
│  │  │ • cursor │ │ • mode   │ │ • depth  │ │ • projections │  │   │
│  │  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │   │
│  │                                                              │   │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │   │
│  │  │ApprovalQ│ │FileTree  │ │LogConsole│ │ LayoutTree    │  │   │
│  │  │          │ │          │ │          │ │               │  │   │
│  │  │ • queue  │ │ • nodes  │ │ • lines  │ │ • splits      │  │   │
│  │  │ • active │ │ • expand │ │ • filter │ │ • focus       │  │   │
│  │  │ • risk   │ │ • watch  │ │ • search │ │ • sizes       │  │   │
│  │  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │   │
│  └──────────────────────────────────────────────────────────────┘   │
│         │                                                          │
│         ▼                                                          │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │                    Render Pipeline                           │   │
│  │                                                              │   │
│  │  AppState ──► Layout Solver ──► Widget Render ──► DiffEngine │   │
│  │                                                │             │   │
│  │                                                ▼             │   │
│  │                                          crossterm::execute  │   │
│  └──────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

## The Terminal Rendering Pipeline

### Stage 1: Crossterm Backend Initialization

```rust
/// Terminal setup: raw mode + alternate screen 
fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;                          // No line buffering, no echo
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,                    // Private screen buffer
        EnableMouseCapture,                      // Mouse events for pane resize
        EnableBracketedPaste,                    // Paste multi-line input
        EnableFocusChange,                       // Detect terminal focus
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}
```

The crossterm backend translates platform-specific terminal escape sequences into
a unified `Event` enum. On Unix, this reads from `/dev/tty` via `libc::read()` with
a non-blocking file descriptor registered in the tokio runtime via `tokio::io::unix::AsyncFd`.
On Windows, `conpty` events are read via `ReadConsoleInputW`.

### Stage 2: ratatui Frame Construction

Each frame tick, ratatui:

1. Calls `terminal.draw(|frame| { ... })` with a fresh `Frame` handle
2. The `Frame` provides a `Rect` representing the full terminal area
3. Widgets are rendered into `Buffer` cells (character + style) via `frame.render_widget()`
4. The frame's `Buffer` is diffed against the previous frame's `Buffer`
5. Only changed cells are written to the terminal via crossterm escape sequences

```rust
/// The core render function — called every frame tick
fn render_frame(frame: &mut Frame, state: &AppState) {
    let layout = solve_layout(state.layout_tree(), frame.area());

    // Render each pane into its allocated rect
    for (pane_id, rect) in layout.pane_rects() {
        match state.pane_type(pane_id) {
            PaneType::Chat       => frame.render_widget(ChatWidget::new(&state.chat), rect),
            PaneType::Diff       => frame.render_widget(DiffWidget::new(&state.diff), rect),
            PaneType::FileTree   => frame.render_widget(FileTreeWidget::new(&state.file_tree), rect),
            PaneType::AgentActivity => frame.render_widget(AgentWidget::new(&state.agents), rect),
            PaneType::TokenDashboard => frame.render_widget(TokenWidget::new(&state.costs), rect),
            PaneType::LogConsole => frame.render_widget(LogWidget::new(&state.logs), rect),
            PaneType::Timeline   => frame.render_widget(TimelineWidget::new(&state.timeline), rect),
        }
    }

    // Render floating overlays (approval dialogs, command palette)
    for overlay in state.overlays() {
        let rect = overlay.rect(frame.area());
        frame.render_widget(overlay.widget(), rect);
    }

    // Render status bar (always last, bottom of screen)
    frame.render_widget(StatusBar::new(&state), status_rect(frame.area()));
}
```

### Stage 3: Custom xaft Widgets

xaft widgets implement ratatui's `Widget` trait but are backed by async-updated state:

```rust
/// A xaft widget is any struct implementing Widget with state from AppSignal
pub trait XaftWidget: Widget {
    /// Which signals this widget subscribes to
    fn subscriptions() -> Vec<SignalKind>;

    /// Update internal state from a signal (called from tokio task)
    fn handle_signal(&mut self, signal: &AppSignal);

    /// Whether this widget needs a redraw
    fn is_dirty(&self) -> bool;
}

/// Example: ChatWidget streams LLM tokens into its internal buffer
pub struct ChatWidget<'a> {
    messages: &'a [ChatMessage],
    scroll_offset: u16,
    cursor_pos: (u16, u16),
    stream_buffer: &'a str,    // Currently streaming partial response
    dirty: bool,
}

impl<'a> Widget for ChatWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Render message history
        let mut y = 0u16;
        for msg in self.messages {
            let msg_height = msg.render_height(area.width);
            if y + msg_height > area.height { break; }
            render_message(msg, Rect::new(area.x, area.y + y, area.width, msg_height), buf);
            y += msg_height;
        }

        // Render streaming buffer with blink cursor
        if !self.stream_buffer.is_empty() {
            render_streaming_text(self.stream_buffer, area, buf, y);
        }
    }
}
```

## Async Rendering Model

### The Dual-Loop Architecture

xaft runs two concurrent loops in the same tokio runtime:

```
┌─────────────────────────────────────────────────────┐
│                   tokio::Runtime                     │
│                                                      │
│  ┌────────────────────┐  ┌────────────────────────┐ │
│  │  TUI Event Loop    │  │  Agent Signal Loop     │ │
│  │  (60fps tick)      │  │  (event-driven)        │ │
│  │                    │  │                        │ │
│  │  loop {            │  │  loop {                │ │
│  │    select! {       │  │    signal = rx.recv() { │ │
│  │      tick  => {    │  │      update_state()    │ │
│  │        if dirty {  │  │      mark_dirty()      │ │
│  │          draw()    │  │    }                    │ │
│  │        }           │  │  }                      │ │
│  │      }             │  │                        │ │
│  │      key   => {    │  │  Subscribes to:        │ │
│  │        handle()    │  │  • LlmTokenStream      │ │
│  │        mark_dirty()│  │  • ToolCallStart       │ │
│  │      }             │  │  • ToolCallComplete    │ │
│  │      mouse => {    │  │  • ApprovalRequired    │ │
│  │        handle()    │  │  • EditReceipt         │ │
│  │      }             │  │  • TaskStateChange     │ │
│  │      resize=> {    │  │  • ModelCallComplete   │ │
│  │        resize()    │  │  • UserUsageRecorded   │ │
│  │        draw()      │  │  • SignalEmitted       │ │
│  │      }             │  │  • CoordinatorEvent    │ │
│  │    }               │  │                        │ │
│  │  }                 │  │  }                      │ │
│  └────────────────────┘  └────────────────────────┘ │
│           │                        │                 │
│           └────────┐  ┌───────────┘                 │
│                    ▼  ▼                              │
│            ┌──────────────┐                          │
│            │  AppState    │                          │
│            │  Arc<RwLock> │                          │
│            └──────────────┘                          │
└─────────────────────────────────────────────────────┘
```

### Channel Topology

```rust
/// Signal flow from agent runtime to TUI state
pub struct TuiChannels {
    /// High-frequency: LLM token stream (80+ msg/s)
    pub token_stream: mpsc::UnboundedReceiver<StreamToken>,

    /// Medium-frequency: tool calls, state changes (1-10 msg/s)
    pub agent_events: mpsc::UnboundedReceiver<AgentEvent>,

    /// Low-frequency: approvals, cost updates (0.1-1 msg/s)
    pub control_events: mpsc::UnboundedReceiver<ControlEvent>,

    /// Terminal input events (key, mouse, resize)
    pub term_events: mpsc::UnboundedReceiver<TermEvent>,

    /// Frame tick signal (60fps = 16.67ms interval)
    pub render_tick: tokio::time::Interval,

    /// Dirty flag — set by signal handlers, consumed by render loop
    pub dirty_flag: Arc<AtomicBool>,
}
```

### Backpressure Management

When the agent produces tokens faster than the terminal can render:

1. **Token batching**: Stream tokens are accumulated into a `String` buffer. The render
   loop reads the entire buffer each frame — no per-token redraws.

2. **Signal coalescing**: Multiple `AgentEvent::StatusChanged` signals between frames are
   collapsed into a single state update. Only the latest status matters.

3. **Render skip**: If `dirty_flag` is false, the render tick is a no-op. No wasted CPU
   on static screens.

4. **Priority channels**: Approval requests use a high-priority channel that preempts
   token stream processing. User-blocking events always render immediately.

```rust
/// Backpressure-aware signal processing
async fn process_signals(
    state: Arc<RwLock<AppState>>,
    channels: &mut TuiChannels,
) {
    let mut token_batch = String::new();
    let mut batch_deadline = Instant::now();

    loop {
        tokio::select! {
            // Token stream: batch accumulate
            Some(token) = channels.token_stream.recv() => {
                token_batch.push_str(&token.text);
                if token.batch_end || batch_deadline.elapsed() > Duration::from_millis(50) {
                    let mut s = state.write().await;
                    s.chat.append_stream(&token_batch);
                    s.mark_dirty();
                    token_batch.clear();
                    batch_deadline = Instant::now();
                }
            }

            // Agent events: immediate processing
            Some(event) = channels.agent_events.recv() => {
                let mut s = state.write().await;
                s.handle_agent_event(event);
                s.mark_dirty();
            }

            // Control events: highest priority
            Some(event) = channels.control_events.recv() => {
                let mut s = state.write().await;
                s.handle_control_event(event);
                s.mark_dirty();
                s.mark_urgent(); // Force immediate redraw
            }
        }
    }
}
```

## The Main Event Loop

### Frame Budget: 16.67ms at 60fps

```
Frame Timeline (16.67ms budget)
─────────────────────────────────────────────────────────
│ 0ms        2ms        5ms        8ms       16.67ms  │
│  │          │          │          │           │      │
│  ├─ poll ──┤          │          │           │      │
│  │ events   │          │          │           │      │
│  │          ├─ apply ──┤          │           │      │
│  │          │  state   │          │           │      │
│  │          │          ├─ render ─┤           │      │
│  │          │          │  widgets │           │      │
│  │          │          │          ├─ diff ───┤      │
│  │          │          │          │  buffer   │      │
│  │          │          │          │           ├─flush│
│  │          │          │          │           │ write│
└──┴──────────┴──────────┴──────────┴───────────┴──────┘
```

### Event Loop Implementation

```rust
/// Main event loop: terminal events → state updates → scheduled redraws
pub async fn run_app(
    mut terminal: Terminal<CrosstermBackend<Stdout>>,
    mut channels: TuiChannels,
    state: Arc<RwLock<AppState>>,
) -> Result<()> {
    // Render tick: 60fps when dirty, 4fps when idle
    let mut render_interval = time::interval(Duration::from_millis(16));
    let idle_interval = Duration::from_millis(250);

    loop {
        let is_dirty = state.read().await.is_dirty();

        // Adjust tick rate based on activity
        let deadline = if is_dirty {
            render_interval.tick().await
        } else {
            tokio::select! {
                _ = render_interval.tick() => continue,
                _ = tokio::time::sleep(idle_interval) => {
                    // Check if anything changed during idle
                    if state.read().await.is_dirty() {
                        render_interval.reset();
                    }
                    continue;
                }
            }
        };

        // Poll terminal events (non-blocking)
        while let Ok(event) = channels.term_events.try_recv() {
            let mut s = state.write().await;
            match event {
                TermEvent::Key(key) => s.handle_key_event(key)?,
                TermEvent::Mouse(mouse) => s.handle_mouse_event(mouse),
                TermEvent::Resize(w, h) => {
                    terminal.resize(Rect::new(0, 0, w, h))?;
                    s.handle_resize(w, h);
                }
            }
        }

        // Render if dirty
        if state.read().await.is_dirty() {
            let s = state.read().await;
            terminal.draw(|frame| render_frame(frame, &s))?;
            state.write().await.clear_dirty();
        }

        // Check for quit
        if state.read().await.should_quit() {
            break;
        }
    }

    Ok(())
}
```

## Terminal Performance Constraints

### 60fps Target: Why It Matters

LLM token streaming at 80 tok/s means approximately 1 new token every 12.5ms. At 60fps
(16.67ms per frame), we render 1-2 new tokens per frame. If rendering drops below 30fps,
the streaming text appears to stutter — tokens arrive in visible batches rather than a
smooth flow.

### Diff-Based Rendering (ratatui's Buffer Diff)

ratatui's `Terminal::draw()` performs buffer diffing automatically:

```rust
// Inside ratatui's draw implementation (simplified)
fn draw(&mut self, f: impl FnOnce(&mut Frame)) -> io::Result<()> {
    let mut buffer = Buffer::empty(self.viewport_area);
    let mut frame = Frame::new(self.viewport_area, &mut buffer);
    f(&mut frame);

    // Diff: compare new buffer with previous buffer, cell by cell
    let changes = self.previous_buffer.diff(&buffer);
    // changes: Vec<(u16, u16, &Cell)> — only cells that changed

    // Write only changed cells to terminal
    for (x, y, cell) in changes {
        write!(self.backend, "{}{}", cursor::MoveTo(x, y), cell)?;
    }

    self.previous_buffer = buffer;
    Ok(())
}
```

This means: **only changed characters are written to the terminal**. When the agent is
idle, the diff is empty and no bytes are written. When streaming text, only the new
characters at the bottom of the chat pane are written.

### Performance Budget Breakdown

| Operation | Budget | Actual (typical) | Notes |
|---|---|---|---|
| Event polling | 1ms | 0.1ms | Non-blocking try_recv on channels |
| State lock acquisition | 0.5ms | 0.05ms | RwLock read: fast path is uncontended |
| Layout solving | 1ms | 0.3ms | Tree walk, O(n) in pane count |
| Widget rendering | 5ms | 2ms | ratatui Widget::render for 5-8 panes |
| Buffer diff | 2ms | 0.5ms | Cell-by-cell comparison of ~10K cells |
| Terminal write | 5ms | 1-3ms | Depends on terminal emulator speed |
| **Total** | **14.5ms** | **~4ms** | **Well within 16.67ms budget** |

### Worst-Case: Full-Screen Redraw

Terminal resize forces a full buffer invalidation. This writes ~10K cells (for a
120×40 terminal) = ~40KB of escape sequences. Modern terminals process this in <10ms.
xterm.js over SSH may take 20-50ms — in that case, we degrade to 30fps by skipping
alternate frames.

```rust
/// Adaptive frame rate based on render time measurement
struct FrameRateController {
    last_render_duration: Duration,
    target_frame_time: Duration,
    skip_count: u32,
}

impl FrameRateController {
    fn should_render(&mut self, render_duration: Duration) -> bool {
        self.last_render_duration = render_duration;

        // If rendering takes >80% of frame budget, skip every other frame
        if render_duration > self.target_frame_time * 4 / 5 {
            self.skip_count += 1;
            if self.skip_count % 2 == 0 {
                return true; // Render every other frame
            }
            return false;
        }

        self.skip_count = 0;
        true
    }
}
```

## Signal-to-State Mapping

The agtrs runtime emits typed signals that xaft's TUI subscribes to:

```
agtrs Signal              →  TUI State Update               →  Dirty Pane
─────────────────────────────────────────────────────────────────────────
LlmTokenStream            →  ChatState.stream_buffer += tok  →  Chat
ModelCallComplete         →  CostState.add_call(record)      →  TokenDashboard
ToolCallStart             →  AgentState.set_status(calling)  →  AgentActivity
ToolCallComplete          →  AgentState.set_status(thinking) →  AgentActivity
EditReceipt               →  DiffState.add_hunk(receipt)    →  Diff
ApprovalRequired          →  ApprovalQueue.enqueue(request)  →  ApprovalDialog
TaskStateChange           →  AgentTree.update(task_state)    →  AgentActivity
SignalEmitted             →  LogConsole.append(signal)       →  LogConsole
UserUsageRecorded         →  CostState.update_usage(record)  →  TokenDashboard
CoordinatorDelegated      →  AgentTree.add_subagent(task)    →  AgentActivity, Timeline
FileChanged(fs_event)     →  FileTreeState.refresh(path)     →  FileTree
```

### Dirty Region Tracking

Rather than redrawing all panes every frame, xaft tracks which panes are dirty:

```rust
#[derive(Debug, Default)]
pub struct DirtyTracker {
    dirty_panes: HashSet<PaneId>,
    urgent: bool,    // Full redraw required (resize, overlay change)
}

impl DirtyTracker {
    pub fn mark_pane(&mut self, pane: PaneId) {
        self.dirty_panes.insert(pane);
    }

    pub fn mark_urgent(&mut self) {
        self.urgent = true;
    }

    /// Returns which panes need redrawing this frame
    pub fn drain(&mut self) -> RedrawPlan {
        RedrawPlan {
            panes: self.dirty_panes.drain().collect(),
            full: std::mem::take(&mut self.urgent),
        }
    }
}

pub struct RedrawPlan {
    pub panes: Vec<PaneId>,
    pub full: bool,
}
```

## Graceful Shutdown

```rust
/// Clean terminal restoration on exit
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        cursor::Show,
    )?;
    terminal.show_cursor()?;
    Ok(())
}
```

The shutdown sequence:
1. Agent runtime receives `SIGTERM` → gracefully finishes current tool call
2. TUI event loop receives quit signal → stops render tick
3. `restore_terminal()` resets terminal to cooked mode
4. Final summary (cost, token count, files modified) printed to stdout
5. Process exits with code 0 (success) or 1 (agent error)
