# TUI Widget Development

## Purpose

The terminal user interface (TUI) is the primary way users interact with xaft in real time. It displays agent activity, tool results, plan progress, file diffs, and approval gates—all within a terminal. This document explains how to build custom widgets for the xaft TUI using the ratatui framework, how to read data from `AppState`, how to register new panes, manage focus, and integrate with the theme system. Whether you're adding a simple status indicator or a complex multi-panel view, this guide covers the complete development lifecycle.

The TUI layer is deliberately decoupled from the agent runtime. It receives data exclusively through `TuiEvent` signals and reads state from `AppState`. This means widgets never directly access the agent, tools, or LLM—they observe and render. This separation ensures the TUI never blocks the agent and can be developed, tested, and modified independently.

## Mental Model

Think of the TUI as a **dashboard of gauges**. Each widget is a gauge that reads a specific set of values from `AppState` and renders them. The `LayoutManager` decides which gauges are visible and where they're positioned. Focus management determines which gauge receives keyboard input. The theme system ensures all gauges look consistent.

```
┌─────────────────────────────────────────────────────────────┐
│ LayoutManager                                               │
│  ┌──────────────────────────┐  ┌────────────────────────┐  │
│  │ ChatWidget (focused)     │  │ PlanProgressWidget     │  │
│  │                          │  │ ■■■■■■□□□□ 60%        │  │
│  │ Agent: I'll add error    │  │ Step 3 of 5            │  │
│  │ handling to process()... │  │                        │  │
│  │                          │  ├────────────────────────┤  │
│  │ > read_file src/main.rs  │  │ ToolActivityWidget     │  │
│  │                          │  │ ● shell (running)      │  │
│  ├──────────────────────────┤  │ ✓ read_file (0.3s)     │  │
│  │ StatusBarWidget          │  │ ✓ write_file (0.1s)    │  │
│  │ Model: gpt-4 | Turn 3/20│  │                        │  │
│  └──────────────────────────┘  └────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘

Data flow:
  SignalBus → TuiEvent → AppState → Widget::render()
```

## Extension Patterns

### Implementing the Widget Trait

Every widget implements ratatui's `Widget` trait, which has a single `render` method. In xaft, widgets take their data from `AppState` rather than storing it internally:

```rust
use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Color},
    widgets::Widget,
    text::{Line, Span},
};

/// Displays a list of recent tool calls with their status and duration.
pub struct ToolActivityWidget<'a> {
    pub tools: &'a [ToolActivityEntry],
    pub theme: &'a Theme,
}

#[derive(Clone)]
pub struct ToolActivityEntry {
    pub tool_name: String,
    pub status: ToolStatus,
    pub duration_ms: Option<u64>,
    pub input_summary: String,
}

#[derive(Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Success,
    Failed,
    PendingApproval,
}

impl<'a> Widget for ToolActivityWidget<'a> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        // Title
        let title = Line::from(Span::styled(
            " Tool Activity ",
            Style::default()
                .fg(self.theme.title_fg)
                .bg(self.theme.title_bg)
                .bold(),
        ));

        let mut lines: Vec<Line> = vec![title];

        for entry in self.tools.iter().take(area.height.saturating_sub(1) as usize) {
            let icon = match entry.status {
                ToolStatus::Running => "●",
                ToolStatus::Success => "✓",
                ToolStatus::Failed => "✗",
                ToolStatus::PendingApproval => "⏳",
            };
            let icon_color = match entry.status {
                ToolStatus::Running => self.theme.running_color,
                ToolStatus::Success => self.theme.success_color,
                ToolStatus::Failed => self.theme.error_color,
                ToolStatus::PendingApproval => self.theme.warning_color,
            };

            let duration_text = entry.duration_ms
                .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
                .unwrap_or_default();

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", icon), Style::default().fg(icon_color)),
                Span::styled(&entry.tool_name, Style::default().fg(self.theme.text_fg)),
                Span::raw(duration_text),
            ]));
        }

        let paragraph = ratatui::widgets::Paragraph::new(lines);
        Widget::render(paragraph, area, buf);
    }
}
```

### Reading Data from AppState

`AppState` is the single source of truth for the TUI. Widgets read from it during `render_frame()`. Never store derived state in the widget itself—always compute it fresh from `AppState` on each render:

```rust
impl AppState {
    pub fn tool_activity_entries(&self) -> Vec<ToolActivityEntry> {
        self.tool_history
            .iter()
            .rev()
            .take(50)
            .map(|entry| ToolActivityEntry {
                tool_name: entry.tool_name.clone(),
                status: entry.status.clone(),
                duration_ms: entry.duration_ms,
                input_summary: entry.input_summary.clone(),
            })
            .collect()
    }
}
```

Then in the render function:

```rust
fn render_tool_activity(f: &mut Frame, area: Rect, state: &AppState) {
    let entries = state.tool_activity_entries();
    let widget = ToolActivityWidget {
        tools: &entries,
        theme: &state.theme,
    };
    f.render_widget(widget, area);
}
```

### Adding a PaneType Variant

Each distinct area of the TUI layout is identified by a `PaneType` enum variant. To add a new pane:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneType {
    Chat,
    PlanProgress,
    ToolActivity,
    DiffView,
    StatusBar,
    MyNewPane,  // Add your new variant
}
```

### Registering in render_frame()

The `render_frame()` function is called on every terminal frame (typically 30-60 fps). It determines which panes are visible and calls the appropriate render function for each:

```rust
pub fn render_frame(f: &mut Frame, state: &mut AppState, layout: &LayoutManager) {
    let chunks = layout.compute(f.size(), state);

    // Render each pane
    if let Some(area) = chunks.get(&PaneType::Chat) {
        render_chat(f, *area, state);
    }
    if let Some(area) = chunks.get(&PaneType::ToolActivity) {
        render_tool_activity(f, *area, state);
    }
    if let Some(area) = chunks.get(&PaneType::MyNewPane) {
        render_my_new_pane(f, *area, state);
    }
    // ... other panes
}
```

### Focus Management with LayoutManager

The `LayoutManager` tracks which pane has keyboard focus. Only one pane can be focused at a time, and focus determines which key bindings are active:

```rust
impl LayoutManager {
    pub fn focus_next(&mut self) {
        let panes = self.visible_panes();
        if let Some(current_idx) = panes.iter().position(|p| *p == self.focused) {
            let next_idx = (current_idx + 1) % panes.len();
            self.focused = panes[next_idx];
        }
    }

    pub fn focus_prev(&mut self) {
        let panes = self.visible_panes();
        if let Some(current_idx) = panes.iter().position(|p| *p == self.focused) {
            let prev_idx = if current_idx == 0 { panes.len() - 1 } else { current_idx - 1 };
            self.focused = panes[prev_idx];
        }
    }

    pub fn is_focused(&self, pane: PaneType) -> bool {
        self.focused == pane
    }
}
```

Use focus state to render a highlighted border around the focused pane:

```rust
let border_style = if layout.is_focused(PaneType::MyNewPane) {
    Style::default().fg(state.theme.focus_border_color)
} else {
    Style::default().fg(state.theme.dim_border_color)
};
```

### Theme Integration

The `Theme` struct centralizes all color and style choices. Widgets should always read colors from the theme rather than hardcoding them:

```rust
pub struct Theme {
    // Text colors
    pub text_fg: Color,
    pub text_dim: Color,
    pub title_fg: Color,
    pub title_bg: Color,

    // Status colors
    pub success_color: Color,
    pub error_color: Color,
    pub warning_color: Color,
    pub running_color: Color,

    // Layout colors
    pub focus_border_color: Color,
    pub dim_border_color: Color,
    pub background: Color,

    // Syntax highlighting
    pub keyword_color: Color,
    pub string_color: Color,
    pub comment_color: Color,
    pub number_color: Color,
}
```

xaft ships with built-in themes (dark, light, monokai, solarized) and supports custom themes via a `theme.toml` config file. Widgets that use theme colors automatically adapt when the user changes themes.

## Common Pitfalls

1. **Hardcoding colors in widgets.** Always use `theme.xxx_color`. Hardcoded colors break when the user switches themes and make the TUI inconsistent.

2. **Storing derived state in the widget struct.** Widget structs should hold references, not own data. If a widget computes and caches data, it can go stale between frames. Always read from `AppState`.

3. **Not handling zero-size areas.** When the terminal is very small, `area` may have zero width or height. Widgets must handle this gracefully—return early if the area is too small to render anything.

4. **Rendering in the signal handler.** Signal handlers update `AppState`; they should never call `render()` directly. Rendering happens on the frame tick, not on every state change. Updating state in the handler and rendering on the next frame ensures smooth, consistent display.

5. **Ignoring focus state for key bindings.** If a key binding (like `j`/`k` for scrolling) applies to a specific pane, check that the pane is focused before handling the key. Otherwise, pressing `j` might scroll two panes simultaneously.

6. **Blocking in the render function.** The render function runs on the main thread. Never do I/O, network calls, or expensive computations inside it. Pre-compute everything in background tasks and store it in `AppState`.

7. **Not adding the PaneType to the layout computation.** A new PaneType variant that isn't included in `LayoutManager::compute()` will never get an area to render in, resulting in an invisible widget.

## Invariants

- **Widgets never access the agent runtime directly.** All data comes from `AppState`, which is updated exclusively by `TuiEvent` handlers.
- **Rendering happens only in `render_frame()`.** Never render from signal handlers, background tasks, or any other code path.
- **Each `PaneType` variant maps to exactly one render function.** Don't render the same pane type in two different places.
- **Focus is exclusive.** Only one pane is focused at a time. Focus cycles through visible panes.
- **Theme colors are used for all styling.** No hardcoded colors in widget code.
- **Widget structs hold references, not owned data.** This ensures widgets always reflect the latest state.

## Examples

### Minimal Widget: Turn Counter

A simple widget that displays the current turn and maximum turns:

```rust
pub struct TurnCounterWidget<'a> {
    pub current: usize,
    pub max: usize,
    pub theme: &'a Theme,
}

impl<'a> Widget for TurnCounterWidget<'a> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if area.width < 20 || area.height < 1 {
            return; // Too small to render
        }

        let progress = self.current as f64 / self.max as f64;
        let bar_width = (area.width as f64 * progress) as u16;
        let bar_color = if progress < 0.5 {
            self.theme.success_color
        } else if progress < 0.8 {
            self.theme.warning_color
        } else {
            self.theme.error_color
        };

        // Progress bar
        for x in 0..area.width {
            let style = if x < bar_width {
                Style::default().fg(bar_color).bg(bar_color)
            } else {
                Style::default().fg(self.theme.text_dim)
            };
            buf.get_mut(area.x + x, area.y).set_symbol("▁").set_style(style);
        }

        // Text overlay
        let text = format!(" Turn {}/{} ", self.current, self.max);
        let span = Span::styled(text, Style::default().fg(self.theme.title_fg));
        let line = Line::from(span);
        line.render(area, buf);
    }
}
```

### Registering the Widget

```rust
// In render_frame():
if let Some(area) = chunks.get(&PaneType::StatusBar) {
    let widget = TurnCounterWidget {
        current: state.current_turn,
        max: state.max_turns,
        theme: &state.theme,
    };
    f.render_widget(widget, *area);
}
```

### Testing Widgets Without a Terminal

```rust
#[test]
fn test_turn_counter_renders() {
    let theme = Theme::default();
    let widget = TurnCounterWidget {
        current: 3,
        max: 10,
        theme: &theme,
    };

    let area = Rect::new(0, 0, 40, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    widget.render(area, &mut buf);

    // Verify the buffer contains expected content
    let content = buf.area.content();
    assert!(content.contains("3/10"));
}
```
