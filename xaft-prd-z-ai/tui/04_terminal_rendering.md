# XAFT Terminal Rendering

## Low-Level Rendering: Smooth Streaming Text in the Terminal

### The Challenge

LLM tokens arrive at 80+ tokens per second. Each token is a fragment of text (a word
fragment, a punctuation mark, a whitespace). The terminal must render these tokens
smoothly, character by character, without visual artifacts:

- No flicker (full-screen clear + redraw would flicker)
- No stutter (batching too many tokens per frame stutters)
- No lag (tokens must appear within one frame of arrival, ~16ms)
- No tearing (partial writes must be atomic)

### Character-by-Character LLM Output Rendering

#### Token Stream Pipeline

```
LLM API ──► SSE Stream ──► agtrs Runtime ──► mpsc Channel ──► TUI State ──► Frame Render
  (network)   (parsing)    (token buffer)   (async)       (batch)      (ratatui)
```

```rust
/// Token stream processor: accumulates tokens between frames
pub struct TokenStreamRenderer {
    /// Accumulated text since last render
    buffer: String,

    /// Currently visible streaming text (what's on screen)
    visible: String,

    /// Cursor position within the streaming text
    cursor: (u16, u16),

    /// Blink state for cursor animation
    blink_state: bool,

    /// Tokens per second (for display)
    tps: f32,

    /// Time of last token received
    last_token_time: Instant,
}

impl TokenStreamRenderer {
    /// Append a new token from the LLM stream
    pub fn push_token(&mut self, token: &str) {
        self.buffer.push_str(token);
        self.last_token_time = Instant::now();
    }

    /// Called once per frame: move buffer contents to visible
    pub fn frame_update(&mut self) {
        if !self.buffer.is_empty() {
            self.visible.push_str(&self.buffer);
            self.buffer.clear();
        }

        // Toggle blink cursor
        self.blink_state = !self.blink_state;
    }

    /// Render the streaming text into the frame buffer
    pub fn render(&self, area: Rect, buf: &mut Buffer, syntax: Option<&[HighlightedLine]>) {
        let lines = self.wrap_text(&self.visible, area.width);

        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= area.height { break; }

            let y = area.y + i as u16;
            let spans = if let Some(syntax) = syntax {
                // Apply syntax highlighting
                Self::highlight_line(line, syntax.get(i))
            } else {
                // Plain text rendering
                vec![Span::raw(line.clone())]
            };

            Line::from(spans).render(Rect::new(area.x, y, area.width, 1), buf);
        }

        // Render blink cursor at end of streaming text
        if self.blink_state {
            let last_line_idx = lines.len().saturating_sub(1);
            let last_line_width = lines.last().map(|l| Self::display_width(l)).unwrap_or(0);
            let cursor_x = area.x + (last_line_width % area.width as usize) as u16;
            let cursor_y = area.y + last_line_idx as u16;

            if cursor_y < area.bottom() {
                let cursor_char = buf.get_mut(cursor_x, cursor_y);
                cursor_char.set_char('█');
                cursor_char.set_style(Style::default().fg(Color::Cyan));
            }
        }
    }

    /// Word-wrap text to fit within given width, respecting Unicode
    fn wrap_text(&self, text: &str, max_width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current_line = String::new();
        let mut current_width = 0usize;

        for ch in text.chars() {
            let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width + ch_width > max_width as usize {
                lines.push(current_line.clone());
                current_line.clear();
                current_width = 0;
            }
            current_line.push(ch);
            current_width += ch_width;
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }

        lines
    }
}
```

### Rendering Budget

#### Per-Frame Time Budget (16.67ms at 60fps)

```
┌────────────────────────────────────────────────────────────┐
│ Frame Budget: 16.67ms                                      │
│                                                            │
│  ┌─ 0-2ms: Event processing ──────────────────────────┐   │
│  │ • Read terminal events (key, mouse, resize)         │   │
│  │ • Read async signals from channels                  │   │
│  │ • Process signal → state updates                    │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                            │
│  ┌─ 2-5ms: Layout solving ────────────────────────────┐   │
│  │ • Walk layout tree                                  │   │
│  │ • Compute pane rects                                │   │
│  │ • Determine dirty panes                             │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                            │
│  ┌─ 5-12ms: Widget rendering ────────────────────────┐   │
│  │ • Chat: streaming text + message history            │   │
│  │ • Diff: syntax-highlighted hunks                    │   │
│  │ • AgentActivity: tree rendering                     │   │
│  │ • TokenDashboard: counters + gauges                 │   │
│  │ • FileTree: directory listing                       │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                            │
│  ┌─ 12-15ms: Buffer diff + terminal write ───────────┐   │
│  │ • Compare current buffer vs previous buffer         │   │
│  │ • Generate minimal escape sequence payload          │   │
│  │ • Write to stdout (crossterm)                       │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                            │
│  ┌─ 15-16.67ms: Idle ────────────────────────────────┐   │
│  │ • Frame padding, ready for next tick                │   │
│  └─────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────┘
```

#### Adaptive Frame Rate

```rust
/// Adaptive frame rate controller
pub struct FrameRateController {
    /// Target frame time (16.67ms for 60fps)
    target: Duration,

    /// Measured render time for last frame
    last_render_time: Duration,

    /// Consecutive slow frames (render time > 80% of target)
    slow_frame_count: u32,

    /// Current effective FPS
    effective_fps: f32,

    /// Whether to skip the next frame
    skip_next: bool,
}

impl FrameRateController {
    pub fn new() -> Self {
        Self {
            target: Duration::from_millis(16),
            last_render_time: Duration::ZERO,
            slow_frame_count: 0,
            effective_fps: 60.0,
            skip_next: false,
        }
    }

    /// Called after each render. Returns true if next frame should render.
    pub fn should_render_next(&mut self, render_time: Duration) -> bool {
        self.last_render_time = render_time;

        // If render took >80% of frame budget, count as slow
        if render_time > self.target * 4 / 5 {
            self.slow_frame_count += 1;
        } else {
            self.slow_frame_count = 0;
        }

        // After 5 consecutive slow frames, drop to 30fps
        if self.slow_frame_count >= 5 {
            self.effective_fps = 30.0;
            self.skip_next = !self.skip_next;
            return !self.skip_next;
        }

        // After 10 consecutive slow frames, drop to 15fps
        if self.slow_frame_count >= 10 {
            self.effective_fps = 15.0;
            self.skip_next = (self.skip_next as u8 + 1) % 3 != 0;
            return !self.skip_next;
        }

        // Recover: if rendering is fast again, restore 60fps
        if self.slow_frame_count == 0 && self.effective_fps < 60.0 {
            self.effective_fps = 60.0;
        }

        true
    }
}
```

## ANSI Color Management

### Color Palette

xaft uses a 16-color base palette for broad terminal compatibility, with 256-color and
true-color extensions when available:

```rust
/// xaft color palette
pub mod colors {
    use ratatui::style::Color;

    // Primary palette (works on any terminal with 16-color support)
    pub const BG: Color = Color::Reset;
    pub const FG: Color = Color::Reset;
    pub const ACCENT: Color = Color::Cyan;
    pub const SUCCESS: Color = Color::Green;
    pub const WARNING: Color = Color::Yellow;
    pub const ERROR: Color = Color::Red;
    pub const MUTED: Color = Color::DarkGray;
    pub const DIM: Color = Color::Gray;

    // Extended palette (true-color, for terminals that support it)
    pub mod extended {
        use ratatui::style::Color;
        pub const SURFACE: Color = Color::Rgb(30, 30, 40);
        pub const OVERLAY: Color = Color::Rgb(40, 40, 55);
        pub const DIFF_ADD_BG: Color = Color::Rgb(20, 60, 30);
        pub const DIFF_REMOVE_BG: Color = Color::Rgb(60, 20, 20);
        pub const STREAMING_FG: Color = Color::Rgb(100, 200, 255);
        pub const THINKING_FG: Color = Color::Rgb(200, 180, 100);
        pub const TOOL_FG: Color = Color::Rgb(180, 100, 255);
    }
}
```

### Terminal Color Capability Detection

```rust
/// Detect terminal color capabilities
pub fn detect_color_support() -> ColorSupport {
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();

    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        ColorSupport::TrueColor
    } else if term.contains("256color") || colorterm.contains("256") {
        ColorSupport::Color256
    } else if term.contains("color") || term.contains("ansi") {
        ColorSupport::Color16
    } else {
        ColorSupport::Monochrome
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorSupport {
    /// No color support at all
    Monochrome,
    /// Basic 16 ANSI colors
    Color16,
    /// 256-color palette
    Color256,
    /// True color (24-bit RGB)
    TrueColor,
}
```

### Style Resolution

```rust
/// Resolve a Style based on terminal capabilities
pub fn resolve_style(style: Style, support: ColorSupport) -> Style {
    match support {
        ColorSupport::TrueColor => style, // Use as-is
        ColorSupport::Color256 => Style {
            fg: style.fg.map(|c| downsample_to_256(c)),
            bg: style.bg.map(|c| downsample_to_256(c)),
            ..style
        },
        ColorSupport::Color16 => Style {
            fg: style.fg.map(|c| downsample_to_16(c)),
            bg: style.bg.map(|c| downsample_to_16(c)),
            ..style
        },
        ColorSupport::Monochrome => Style {
            fg: None,
            bg: None,
            add_modifier: style.add_modifier | Modifier::BOLD | Modifier::REVERSED,
            ..style
        },
    }
}

/// Downsample Rgb to nearest 256-color index
fn downsample_to_256(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            // Map to 6x6x6 color cube (indices 16-231)
            let ri = (r as f32 / 51.0).round() as u8;
            let gi = (g as f32 / 51.0).round() as u8;
            let bi = (b as f32 / 51.0).round() as u8;
            Color::Indexed(16 + 36 * ri + 6 * gi + bi)
        }
        other => other,
    }
}
```

## Tool Execution Progress Display

### Inline Tool Progress

When the agent executes a tool, xaft shows progress inline in the chat pane:

```
┌──────────────────────────────────────────────────────────┐
│ 🤖 I'll fix the auth bug. Let me read the file first.    │
│                                                          │
│ ⚙ ReadFile("src/auth/token.rs") ─── ✓ done (0.3s)       │
│                                                          │
│ ⚙ Bash("cargo check") ─── ● running... 2.1s             │
│   ╭─────────────────────────────────────────────────╮    │
│   │ Compiling xaft v0.1.0                           │    │
│   │   ████████████████░░░░░░  67%  [156/232 crates] │    │
│   ╰─────────────────────────────────────────────────╯    │
│                                                          │
│ ⚙ FileEditor("src/auth/token.rs") ─── awaiting approval │
└──────────────────────────────────────────────────────────┘
```

### Tool Call Widget

```rust
/// Inline tool call display widget
pub struct ToolCallWidget<'a> {
    tool_name: &'a str,
    tool_input: &'a str,
    status: ToolCallStatus,
    duration: Duration,
    progress: Option<Progress>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolCallStatus {
    Pending,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
}

impl<'a> Widget for ToolCallWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (icon, color) = match self.status {
            ToolCallStatus::Pending           => ("○", Color::DarkGray),
            ToolCallStatus::Running           => ("●", Color::Yellow),
            ToolCallStatus::AwaitingApproval  => ("⚠", Color::Magenta),
            ToolCallStatus::Completed         => ("✓", Color::Green),
            ToolCallStatus::Failed            => ("✗", Color::Red),
        };

        // Tool name + truncated input
        let input_display = truncate_with_ellipsis(self.tool_input, (area.width as usize).saturating_sub(30));
        let header = Line::from(vec![
            Span::styled(format!(" {} ", icon), Style::default().fg(color)),
            Span::styled(self.tool_name.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(format!("({})", input_display), Style::default().fg(Color::Gray)),
            Span::raw(" "),
            Span::styled(
                match self.status {
                    ToolCallStatus::Running => format!("● running... {:.1}s", self.duration.as_secs_f64()),
                    ToolCallStatus::AwaitingApproval => "awaiting approval".into(),
                    ToolCallStatus::Completed => format!("✓ done ({:.1}s)", self.duration.as_secs_f64()),
                    ToolCallStatus::Failed => format!("✗ failed ({:.1}s)", self.duration.as_secs_f64()),
                    ToolCallStatus::Pending => "pending".into(),
                },
                Style::default().fg(color),
            ),
        ]);
        header.render(area, buf);

        // Progress bar (if present)
        if let Some(progress) = self.progress {
            let gauge = LineGauge::default()
                .gauge_style(Style::default().fg(color).bg(Color::DarkGray))
                .ratio(progress.fraction())
                .label(Span::raw(progress.label()));
            gauge.render(Rect::new(area.x + 2, area.y + 1, area.width - 4, 1), buf);
        }
    }
}
```

## Spinner/Progress Indicators

### Spinner Animation

```rust
/// ASCII spinner frames (cycling at 10fps)
const SPINNER_FRAMES: &[&str] = &[
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];

/// Simple ASCII fallback (for terminals without Unicode)
const SPINNER_ASCII: &[&str] = &[
    "|", "/", "-", "\\",
];

pub struct Spinner {
    frame_index: usize,
    last_update: Instant,
    interval: Duration,
    unicode: bool,
}

impl Spinner {
    pub fn new(unicode: bool) -> Self {
        Self {
            frame_index: 0,
            last_update: Instant::now(),
            interval: Duration::from_millis(100), // 10fps
            unicode,
        }
    }

    pub fn current_frame(&mut self) -> &str {
        if self.last_update.elapsed() >= self.interval {
            self.frame_index = (self.frame_index + 1) % self.frames().len();
            self.last_update = Instant::now();
        }
        self.frames()[self.frame_index]
    }

    fn frames(&self) -> &[&str] {
        if self.unicode { SPINNER_FRAMES } else { SPINNER_ASCII }
    }
}
```

### Progress Bar Styles

```
Determinate:    ████████████░░░░░░░░  62%  [124/200]
Indeterminate:  ████░░░░░░░░░░░░████  (scrolling)
Subtasks:       ██████░░░░  62%       │ 4/6 subtasks complete
                  ├─ ✓ ReadFile
                  ├─ ✓ EditFile
                  ├─ ● Bash (running)
                  └─ ○ WriteFile (pending)
```

```rust
/// Determinate progress with subtask tracking
pub struct SubtaskProgress {
    total: usize,
    completed: usize,
    running: usize,
    pending: usize,
    fraction: f64,
    subtasks: Vec<SubtaskStatus>,
}

#[derive(Debug, Clone)]
pub struct SubtaskStatus {
    name: String,
    state: ToolCallStatus,
    duration: Duration,
}

impl Widget for &SubtaskProgress {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Main progress bar
        let label = format!(" {}/{} subtasks complete ", self.completed, self.total);
        let gauge = LineGauge::default()
            .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
            .ratio(self.fraction)
            .label(label);
        gauge.render(Rect::new(area.x, area.y, area.width, 1), buf);

        // Subtask list (indented)
        for (i, subtask) in self.subtasks.iter().enumerate() {
            let y = area.y + 1 + i as u16;
            if y >= area.bottom() { break; }

            let (icon, color) = match subtask.state {
                ToolCallStatus::Completed => ("✓", Color::Green),
                ToolCallStatus::Running   => ("●", Color::Yellow),
                ToolCallStatus::Pending   => ("○", Color::DarkGray),
                ToolCallStatus::Failed    => ("✗", Color::Red),
                ToolCallStatus::AwaitingApproval => ("⚠", Color::Magenta),
            };

            let connector = if i < self.subtasks.len() - 1 { "├─" } else { "└─" };
            let line = Line::from(vec![
                Span::raw("   "),
                Span::styled(connector, Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {} ", icon), Style::default().fg(color)),
                Span::styled(subtask.name.clone(), Style::default().fg(Color::Gray)),
            ]);
            line.render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}
```

## Terminal Resize Handling

### Resize Event Flow

```
Terminal emulator                  xaft TUI
─────────────────                  ─────────
User drags corner  ────────►  SIGWINCH signal
                              │
                              ▼
                         crossterm reads
                         new terminal size
                              │
                              ▼
                         TermEvent::Resize(w, h)
                              │
                              ▼
                         ┌────────────────────────┐
                         │ 1. terminal.resize()    │
                         │ 2. Layout re-solve      │
                         │ 3. Scroll clamp         │
                         │ 4. Full redraw          │
                         │ 5. Persist new size     │
                         └────────────────────────┘
```

```rust
/// Handle terminal resize
fn handle_resize(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &mut AppState,
    new_size: (u16, u16),
) -> Result<()> {
    // 1. Update terminal size
    terminal.resize(Rect::new(0, 0, new_size.0, new_size.1))?;

    // 2. Recalculate layout
    state.terminal_size = new_size;
    let new_rect = Rect::new(0, 0, new_size.0, new_size.1);

    // Check minimum size requirements
    if new_size.0 < 60 || new_size.1 < 25 {
        state.show_size_warning = true;
        return Ok(());
    }
    state.show_size_warning = false;

    // 3. Re-solve layout (ratios are preserved, absolute sizes change)
    state.layout_solution = solve_layout(&state.layout_tree, new_rect);

    // 4. Clamp scroll positions to new content boundaries
    for (pane_id, rect) in state.layout_solution.pane_rects() {
        let (sx, sy) = state.scroll_positions.get(&pane_id).copied().unwrap_or((0, 0));
        let max_y = state.content_height(pane_id).saturating_sub(rect.height);
        state.scroll_positions.insert(pane_id, (
            sx.min(rect.width),
            sy.min(max_y as u16),
        ));
    }

    // 5. Mark urgent (full redraw needed after resize)
    state.mark_urgent();

    Ok(())
}
```

### Debounced Resize

Terminal emitters often send multiple resize events in quick succession during a
drag operation. xaft debounces these:

```rust
/// Debounce resize events (wait 50ms for stability)
pub struct ResizeDebouncer {
    last_resize: Option<(u16, u16, Instant)>,
    debounce_ms: u64,
}

impl ResizeDebouncer {
    pub fn new() -> Self {
        Self {
            last_resize: None,
            debounce_ms: 50,
        }
    }

    /// Returns Some((w, h)) when the resize has stabilized
    pub fn observe(&mut self, w: u16, h: u16) -> Option<(u16, u16)> {
        let now = Instant::now();
        match self.last_resize {
            Some((lw, lh, t)) if lw == w && lh == h && now.duration_since(t) > Duration::from_millis(self.debounce_ms) => {
                self.last_resize = None;
                Some((w, h))
            }
            Some((lw, lh, _)) if lw == w && lh == h => {
                None // Same size, still debouncing
            }
            _ => {
                // New size, start debounce
                self.last_resize = Some((w, h, now));
                None
            }
        }
    }
}
```

## Unicode/Emoji Support

### Unicode Width Handling

Terminals have inconsistent Unicode width support. xaft handles this carefully:

```rust
/// Unicode-aware text measurement
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Truncate text to fit within a given display width
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut width = 0;

    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }

    // Add ellipsis if truncated
    if width + 3 <= max_width && result.len() < text.len() {
        result.push('…');
    }

    result
}
```

### Emoji Rendering Strategy

| Emoji | Unicode | Fallback | Usage |
|---|---|---|---|
| ✓ | U+2713 | `[v]` | Completed tool call |
| ✗ | U+2717 | `[x]` | Failed tool call |
| ● | U+25CF | `*` | Running indicator |
| ○ | U+25CB | `o` | Pending indicator |
| ⚠ | U+26A0 | `[!]` | Warning/approval needed |
| ⚙ | U+2699 | `[tool]` | Tool execution |
| ⏎ | U+23CE | `<Enter>` | Input submit |
| 🤖 | U+1F916 | `[AI]` | Agent message |
| ┃ | U+2503 | `\|` | Vertical border |
| ━ | U+2501 | `-` | Horizontal border |

```rust
/// Symbol resolver: picks Unicode or ASCII fallback based on terminal capability
pub struct SymbolSet {
    pub check: &'static str,
    pub cross: &'static str,
    pub bullet: &'static str,
    pub circle: &'static str,
    pub warning: &'static str,
    pub gear: &'static str,
    pub agent: &'static str,
    pub vborder: &'static str,
    pub hborder: &'static str,
    pub ellipsis: &'static str,
    pub spinner: &'static [&'static str],
}

pub const UNICODE_SYMBOLS: SymbolSet = SymbolSet {
    check: "✓",
    cross: "✗",
    bullet: "●",
    circle: "○",
    warning: "⚠",
    gear: "⚙",
    agent: "🤖",
    vborder: "┃",
    hborder: "━",
    ellipsis: "…",
    spinner: SPINNER_FRAMES,
};

pub const ASCII_SYMBOLS: SymbolSet = SymbolSet {
    check: "[v]",
    cross: "[x]",
    bullet: "*",
    circle: "o",
    warning: "[!]",
    gear: "[tool]",
    agent: "[AI]",
    vborder: "|",
    hborder: "-",
    ellipsis: "...",
    spinner: SPINNER_ASCII,
};
```

### Unicode Detection

```rust
/// Detect if the terminal supports Unicode
pub fn detect_unicode_support() -> bool {
    let lang = std::env::var("LANG").unwrap_or_default();
    let lc_all = std::env::var("LC_ALL").unwrap_or_default();
    let lc_ctype = std::env::var("LC_CTYPE").unwrap_or_default();

    let locale = lc_all.or_else(|_| lc_ctype).or_else(|_| lang).unwrap_or_default();

    // Check for UTF-8 locale
    locale.to_lowercase().contains("utf-8") || locale.to_lowercase().contains("utf8")
}
```

## Dirty Region Tracking

### Per-Pane Dirty Tracking

Rather than redrawing all panes every frame, xaft tracks which panes have changed:

```rust
/// Dirty region tracker
#[derive(Debug, Default)]
pub struct DirtyTracker {
    /// Panes that need redraw
    dirty_panes: HashSet<PaneId>,

    /// Whether a full redraw is needed (resize, overlay change)
    full_redraw: bool,

    /// Specific regions within panes that changed (for large panes)
    partial_dirty: HashMap<PaneId, Vec<Rect>>,
}

impl DirtyTracker {
    /// Mark a pane as needing full redraw
    pub fn mark_pane(&mut self, pane: PaneId) {
        self.dirty_panes.insert(pane);
        self.partial_dirty.remove(&pane); // Full redraw supersedes partial
    }

    /// Mark a specific region within a pane
    pub fn mark_region(&mut self, pane: PaneId, region: Rect) {
        self.partial_dirty.entry(pane).or_default().push(region);
    }

    /// Mark everything dirty (resize, mode change)
    pub fn mark_full(&mut self) {
        self.full_redraw = true;
        self.dirty_panes.clear();
        self.partial_dirty.clear();
    }

    /// Get the redraw plan for this frame
    pub fn plan(&self) -> RedrawPlan {
        if self.full_redraw {
            RedrawPlan::FullRedraw
        } else if self.dirty_panes.is_empty() && self.partial_dirty.is_empty() {
            RedrawPlan::Skip
        } else {
            let panes = self.dirty_panes.iter()
                .chain(self.partial_dirty.keys())
                .copied()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            RedrawPlan::PartialRedraw { panes }
        }
    }
}

pub enum RedrawPlan {
    /// No changes, skip this frame
    Skip,
    /// Redraw specific panes only
    PartialRedraw { panes: Vec<PaneId> },
    /// Redraw everything
    FullRedraw,
}
```

### Rendering with Dirty Tracking

```rust
/// Render only dirty panes (optimization for static screens)
fn render_with_dirty_tracking(
    frame: &mut Frame,
    state: &AppState,
    plan: &RedrawPlan,
) {
    match plan {
        RedrawPlan::Skip => {
            // No rendering needed — the previous frame's buffer is still valid
        }
        RedrawPlan::FullRedraw => {
            // Render all panes
            render_frame(frame, state);
        }
        RedrawPlan::PartialRedraw { panes } => {
            // Render only dirty panes
            for pane_id in panes {
                if let Some(rect) = state.layout_solution.pane_rect(*pane_id) {
                    let pane_type = state.pane_type(*pane_id);
                    render_single_pane(frame, state, *pane_id, pane_type, rect);
                }
            }
        }
    }
}
```

## Streaming Text Performance Optimization

### Token Buffering Strategy

```
High-frequency token stream (80 tok/s = 12.5ms per token)

  Token arrives: t=0ms  t=12ms  t=25ms  t=37ms  t=50ms
                 │       │       │       │       │
                 ▼       ▼       ▼       ▼       ▼
  Buffer:     "I'll" " fix" " the" " auth" " bug"
                                │
                                ▼
  Frame render (t=16.67ms):    "I'll fix the"
  Frame render (t=33.33ms):   "I'll fix the auth"
  Frame render (t=50ms):      "I'll fix the auth bug"

  → User sees 1-3 new tokens per frame. Smooth streaming.
```

### Large Output Handling

When the agent produces very large outputs (e.g., entire file contents), xaft
optimizes rendering:

1. **Virtual scrolling**: Only render visible lines. A 10,000-line output only
   renders the 40 visible lines.

2. **Lazy syntax highlighting**: Syntax-highlight only visible lines + 20-line
   buffer above/below.

3. **Content truncation**: Lines exceeding the pane width are truncated with
   horizontal scroll on demand.

```rust
/// Virtual scrolling for large content
pub struct VirtualScroll {
    total_lines: usize,
    visible_lines: u16,
    scroll_offset: usize,
    /// Buffer: extra lines rendered but off-screen (for smooth scrolling)
    buffer_lines: usize,
}

impl VirtualScroll {
    /// Get the range of lines to render
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let start = self.scroll_offset.saturating_sub(self.buffer_lines);
        let end = (self.scroll_offset + self.visible_lines as usize + self.buffer_lines)
            .min(self.total_lines);
        start..end
    }

    /// Scroll down by N lines
    pub fn scroll_down(&mut self, n: usize) {
        let max_offset = self.total_lines.saturating_sub(self.visible_lines as usize);
        self.scroll_offset = (self.scroll_offset + n).min(max_offset);
    }

    /// Scroll to make a specific line visible
    pub fn scroll_to_line(&mut self, line: usize) {
        if line < self.scroll_offset {
            self.scroll_offset = line;
        } else if line >= self.scroll_offset + self.visible_lines as usize {
            self.scroll_offset = line.saturating_sub(self.visible_lines as usize / 2);
        }
    }
}
```
