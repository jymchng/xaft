# XAFT Layout Engine

## Pane System Overview

xaft divides the terminal into a tree of resizable panes. Each pane hosts a single
widget type (Chat, Diff, FileTree, etc.) and owns its own scroll state, focus state,
and resize constraints. The layout engine solves the tree into concrete `Rect` regions
each frame.

### Design Goals

1. **No wasted space**: Every cell belongs to a pane. No floating gaps.
2. **Keyboard-first resize**: Alt+HJKL resizes the focused split without mouse.
3. **Dynamic panes**: Panes appear when relevant (Diff appears on EditReceipt, Approval
   appears on requires_confirmation) and disappear when no longer needed.
4. **Stateful persistence**: Scroll positions, fold states, and focus survive resize.
5. **Predictable**: Same layout tree always produces same pixel output. Deterministic.

## Layout Tree

### Node Types

```rust
/// The layout tree is a binary tree of splits and leaves.
#[derive(Debug, Clone)]
pub enum LayoutNode {
    /// Internal node: splits the rect into two children
    Split {
        direction: SplitDirection,
        ratio: f32,           // 0.0-1.0, fraction for first child
        min_sizes: (u16, u16), // (first_child_min, second_child_min) in cells
        children: Box<(LayoutNode, LayoutNode)>,
    },
    /// Leaf node: a single pane
    Pane {
        id: PaneId,
        pane_type: PaneType,
        min_size: (u16, u16),  // (min_width, min_height)
        visible: bool,
        priority: PanePriority,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitDirection {
    Horizontal,  // Left | Right
    Vertical,    // Top | Bottom
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanePriority {
    Critical,   // Chat — always visible, never auto-hidden
    High,       // AgentActivity — visible when agents are running
    Medium,     // Diff, FileTree — visible when relevant data exists
    Low,        // LogConsole, Timeline — visible on demand
}
```

### Default Layout Tree

```
┌──────────────────────────────────────────────────────────────────┐
│  xaft — autonomous coding agent                                  │
├────────────────────────────────────┬─────────────────────────────┤
│                                    │  Agent Activity             │
│                                    │  ┌───────────────────────┐ │
│                                    │  │ ● Coordinator         │ │
│                                    │  │   ├─ ● FileEditor     │ │
│                                    │  │   └─ ○ BashRunner     │ │
│          Chat Pane                 │  │     (waiting)         │ │
│                                    │  └───────────────────────┘ │
│  ┌──────────────────────────────┐  │─────────────────────────────│
│  │ User: fix the auth bug       │  │  Token Dashboard           │
│  │ Agent: I'll start by...      │  │  Tokens: 12,450            │
│  │ ● Tool: ReadFile("auth.rs")  │  │  Cost:   $0.08             │
│  │ ● Tool: EditFile(...)        │  │  Budget: $5.00 remaining   │
│  │ Agent: The bug is in...      │  │  Model:  claude-sonnet     │
│  │ █streaming...                │  │                             │
│  └──────────────────────────────┘  ├─────────────────────────────┤
│                                    │  File Tree                  │
│  ──────────────────────────────    │  src/                       │
│  > Fix the auth token expiry      │    auth/                     │
│    bug in the login handler       │      mod.rs  ← modified     │
│                                    │      token.rs               │
│                                    │    main.rs                  │
│                                    │  Cargo.toml                 │
├────────────────────────────────────┴─────────────────────────────┤
│  ESC quit │ Tab next pane │ Alt+HJKL resize │ : command         │
└──────────────────────────────────────────────────────────────────┘
```

### Tree Representation

```
Split(Horizontal, 0.65)
├── Split(Vertical, 0.78)
│   ├── Pane(Chat, Critical)
│   └── Pane(InputBar, Critical)
└── Split(Vertical, 0.35)
    ├── Split(Vertical, 0.40)
    │   ├── Pane(AgentActivity, High)
    │   └── Pane(TokenDashboard, High)
    ├── Pane(FileTree, Medium)
    └── Pane(StatusBar, Critical)
```

## Layout Solver

### Algorithm

The solver walks the tree top-down, allocating `Rect` regions:

```rust
pub fn solve_layout(node: &LayoutNode, rect: Rect) -> LayoutSolution {
    let mut solution = LayoutSolution::new();
    solve_recursive(node, rect, &mut solution);
    solution
}

fn solve_recursive(node: &LayoutNode, rect: Rect, solution: &mut LayoutSolution) {
    match node {
        LayoutNode::Split { direction, ratio, min_sizes, children } => {
            let (first_rect, second_rect) = match direction {
                SplitDirection::Horizontal => {
                    let split_x = calculate_split(rect.width, *ratio, min_sizes.0, min_sizes.1);
                    (
                        Rect::new(rect.x, rect.y, split_x, rect.height),
                        Rect::new(rect.x + split_x + 1, rect.y, rect.width - split_x - 1, rect.height),
                    )
                }
                SplitDirection::Vertical => {
                    let split_y = calculate_split(rect.height, *ratio, min_sizes.0, min_sizes.1);
                    (
                        Rect::new(rect.x, rect.y, rect.width, split_y),
                        Rect::new(rect.x, rect.y + split_y + 1, rect.width, rect.height - split_y - 1),
                    )
                }
            };
            solve_recursive(&children.0, first_rect, solution);
            solve_recursive(&children.1, second_rect, solution);
        }
        LayoutNode::Pane { id, pane_type, visible, .. } => {
            if *visible {
                solution.set_rect(*id, *pane_type, rect);
            }
        }
    }
}

/// Calculate split position respecting minimum sizes
fn calculate_split(total: u16, ratio: f32, min_first: u16, min_second: u16) -> u16 {
    let ideal = (total as f32 * ratio).round() as u16;
    // Clamp: respect minimum sizes
    let clamped = ideal.max(min_first).min(total.saturating_sub(min_second + 1));
    clamped
}
```

### Border Drawing

Split borders consume 1 cell row/column. The border is shared between adjacent panes:

```rust
/// Draw split borders with focus indicator
fn draw_split_border(buf: &mut Buffer, rect: Rect, direction: SplitDirection, focused: bool) {
    let style = if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    match direction {
        SplitDirection::Horizontal => {
            // Vertical line at rect.x - 1 (right edge of left pane)
            for y in rect.top()..rect.bottom() {
                buf.get_mut(rect.x.saturating_sub(1), y)
                   .set_symbol("│")
                   .set_style(style);
            }
        }
        SplitDirection::Vertical => {
            // Horizontal line at rect.y - 1 (bottom edge of top pane)
            for x in rect.left()..rect.right() {
                buf.get_mut(x, rect.y.saturating_sub(1))
                   .set_symbol("─")
                   .set_style(style);
            }
        }
    }
}
```

## Pane Types

### Complete Pane Type Reference

| Pane Type | Min Size | Default Priority | Auto-show Trigger | Auto-hide Condition |
|---|---|---|---|---|
| Chat | 40×10 | Critical | Always | Never |
| InputBar | 40×3 | Critical | Always | Never |
| Diff | 50×12 | Medium | EditReceipt signal | No pending hunks + 30s idle |
| FileTree | 25×10 | Medium | FileChanged signal | No file changes + 60s idle |
| AgentActivity | 30×8 | High | TaskStateChange signal | No active agents + 10s idle |
| TokenDashboard | 30×6 | High | ModelCallComplete signal | Never (always relevant) |
| LogConsole | 40×8 | Low | User opens (Ctrl+L) | User closes |
| Timeline | 50×10 | Low | User opens (Ctrl+T) | User closes |
| ApprovalDialog | 50×12 | Critical | ApprovalRequired signal | Approval resolved |

### Pane Content Model

```rust
/// Each pane type has its own state struct
pub trait PaneContent: Send + Sync {
    /// Handle a signal that may affect this pane
    fn handle_signal(&mut self, signal: &AppSignal);

    /// Render into the given rect
    fn render(&self, area: Rect, buf: &mut Buffer, focused: bool);

    /// Handle a key event (only when focused)
    fn handle_key(&mut self, key: KeyEvent) -> KeyResult;

    /// Current scroll position for persistence
    fn scroll_position(&self) -> (u16, u16);

    /// Restore scroll position after resize
    fn set_scroll_position(&mut self, x: u16, y: u16);
}
```

## How Panes Resize

### Interactive Resize with Keyboard

```
Before resize (ratio=0.65):              After Alt+L (ratio=0.72):
┌──────────────┬────────┐               ┌────────────────┬──────┐
│              │        │               │                │      │
│   Chat       │ Agents │               │   Chat         │Agent │
│   (65%)      │ (35%)  │   Alt+L      │   (72%)        │(28%) │
│              │        │   ──────►     │                │      │
│              │        │               │                │      │
└──────────────┴────────┘               └────────────────┴──────┘
```

```rust
/// Resize handling: modify the ratio of the focused split
pub fn handle_resize_key(state: &mut AppState, key: KeyEvent) {
    let step = 0.03; // 3% per keypress

    match key.modifiers {
        KeyModifiers::ALT => {
            let focused_split = state.layout_tree.find_split_for_pane(state.focused_pane);
            if let Some(split_id) = focused_split {
                let node = state.layout_tree.get_split_mut(split_id);
                match key.code {
                    KeyCode::Char('h') => node.ratio = (node.ratio - step).max(0.15),
                    KeyCode::Char('l') => node.ratio = (node.ratio + step).min(0.85),
                    KeyCode::Char('j') if node.direction == SplitDirection::Vertical =>
                        node.ratio = (node.ratio + step).min(0.85),
                    KeyCode::Char('k') if node.direction == SplitDirection::Vertical =>
                        node.ratio = (node.ratio - step).max(0.15),
                    _ => {}
                }
                state.mark_dirty();
            }
        }
        _ => {}
    }
}
```

### Mouse Resize (Drag Split Border)

```rust
/// Handle mouse drag on split borders
pub fn handle_mouse_drag(state: &mut AppState, event: &MouseEvent) {
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Check if click is on a split border (within 1 cell)
            if let Some(split_id) = state.layout_tree.find_border_at(event.column, event.row) {
                state.dragging_split = Some((split_id, event.column, event.row));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((split_id, start_x, start_y)) = state.dragging_split {
                let node = state.layout_tree.get_split_mut(split_id);
                let total = match node.direction {
                    SplitDirection::Horizontal => state.terminal_size.0,
                    SplitDirection::Vertical => state.terminal_size.1,
                };
                let offset = match node.direction {
                    SplitDirection::Horizontal => event.column as f32,
                    SplitDirection::Vertical => event.row as f32,
                };
                node.ratio = (offset / total as f32).clamp(0.15, 0.85);
                state.mark_dirty();
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            state.dragging_split = None;
        }
        _ => {}
    }
}
```

### Automatic Pane Sizing

When panes auto-show or auto-hide, the layout tree adjusts ratios proportionally:

```rust
/// Auto-show a pane: redistribute space from neighbors
pub fn auto_show_pane(tree: &mut LayoutTree, pane_id: PaneId) {
    // Find the split containing this pane
    let parent_split = tree.find_parent_split(pane_id);

    // Calculate how much space to take from existing panes
    let existing_count = parent_split.visible_child_count();
    let new_ratio = 1.0 / (existing_count + 1) as f32;

    // Shrink existing panes proportionally
    parent_split.redistribute_with_new(pane_id, new_ratio);

    tree.set_visible(pane_id, true);
}

/// Auto-hide a pane: redistribute space to neighbors
pub fn auto_hide_pane(tree: &mut LayoutTree, pane_id: PaneId) {
    let parent_split = tree.find_parent_split(pane_id);

    // Give this pane's space to its sibling
    parent_split.collapse_child(pane_id);

    tree.set_visible(pane_id, false);
}
```

## Keyboard-First Navigation Between Panes

### Tab Cycling

```
Tab order in default layout:

  ┌─────────────┬──────────┐
  │             │ ② Agents │
  │ ① Chat      │──────────│
  │             │ ③ Tokens │
  │             │──────────│
  │             │ ④ Files  │
  └─────────────┴──────────┘

  Tab: ① → ② → ③ → ④ → ①
  S-Tab: ① → ④ → ③ → ② → ①
```

### Directional Navigation

```
Ctrl+H/J/K/L navigation:

  ┌─────────────┬──────────┐
  │             │ Agents   │
  │ Chat  ◄────┤  ▲       │
  │   │        │  │       │
  │   ▼        │  ▼       │
  │        ────┼────► Files│
  └─────────────┴──────────┘

  From Chat: Ctrl+L → Agents, Ctrl+J → InputBar
  From Agents: Ctrl+H → Chat, Ctrl+J → Tokens
```

```rust
/// Directional pane navigation
pub fn navigate_directional(state: &mut AppState, direction: Direction) {
    let current_rect = state.layout_solution.pane_rect(state.focused_pane);
    let candidates = state.layout_solution.visible_panes();

    // Find the nearest pane in the given direction
    let nearest = candidates
        .iter()
        .filter(|(id, rect)| *id != state.focused_pane)
        .filter(|(_, rect)| is_in_direction(current_rect, rect, direction))
        .min_by_key(|(_, rect)| distance_between(current_rect, rect, direction));

    if let Some((id, _)) = nearest {
        state.focused_pane = *id;
        state.mark_dirty();
    }
}

fn is_in_direction(from: Rect, to: Rect, dir: Direction) -> bool {
    match dir {
        Direction::Left  => to.right() <= from.left(),
        Direction::Right => to.left() >= from.right(),
        Direction::Up    => to.bottom() <= from.top(),
        Direction::Down  => to.top() >= from.bottom(),
    }
}

fn distance_between(from: Rect, to: Rect, dir: Direction) -> u32 {
    match dir {
        Direction::Left  => (from.left() - to.right()) as u32,
        Direction::Right => (to.left() - from.right()) as u32,
        Direction::Up    => (from.top() - to.bottom()) as u32,
        Direction::Down  => (to.top() - from.bottom()) as u32,
    }
}
```

### Focus Indicators

```rust
/// Render focus indicator on pane border
fn render_pane_border(buf: &mut Buffer, rect: Rect, focused: bool, title: &str) {
    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {} ", title),
            if focused {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            }
        ));

    block.render(rect, buf);
}
```

## Pane State Persistence

### State Serialization

```rust
/// Persistent pane state: survives app restarts and resize events
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedPaneState {
    /// Layout tree structure (split ratios, pane types)
    pub layout_tree: LayoutNode,

    /// Per-pane scroll positions
    pub scroll_positions: HashMap<PaneId, (u16, u16)>,

    /// Per-pane fold/expand states (e.g., FileTree expanded dirs)
    pub fold_states: HashMap<PaneId, FoldState>,

    /// Focused pane
    pub focused_pane: Option<PaneId>,

    /// Custom pane order for Tab cycling
    pub tab_order: Vec<PaneId>,
}

impl PersistedPaneState {
    /// Save to XDG config directory
    pub fn save(&self) -> Result<()> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"));
        let path = config_dir.join("xaft").join("tui-state.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Load from XDG config directory
    pub fn load() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"));
        let path = config_dir.join("xaft").join("tui-state.json");
        if path.exists() {
            let json = fs::read_to_string(path)?;
            Ok(serde_json::from_str(&json)?)
        } else {
            Ok(Self::default())
        }
    }
}
```

### Resize-Aware State

When the terminal resizes, pane state must adapt without data loss:

```rust
/// Handle terminal resize while preserving scroll and fold state
pub fn handle_resize(state: &mut AppState, new_width: u16, new_height: u16) {
    let old_size = (state.terminal_size.0, state.terminal_size.1);
    state.terminal_size = (new_width, new_height);

    // Recalculate layout with preserved ratios
    // Ratios are percentage-based, so they survive resize
    state.layout_solution = solve_layout(&state.layout_tree, Rect::new(0, 0, new_width, new_height));

    // Adjust scroll positions to keep visible content stable
    for (pane_id, (scroll_x, scroll_y)) in state.scroll_positions.clone() {
        if let Some(rect) = state.layout_solution.pane_rect(pane_id) {
            // If the pane got smaller, scroll might be beyond content
            let content_height = state.content_height(pane_id);
            let max_scroll = content_height.saturating_sub(rect.height);
            state.scroll_positions.insert(pane_id, (
                scroll_x.min(rect.width),
                scroll_y.min(max_scroll),
            ));
        }
    }

    state.mark_urgent(); // Full redraw required
}
```

## Dynamic Layout Modes

### Preset Layouts

| Preset | Description | Split Config |
|---|---|---|
| `default` | Chat 65% + sidebar 35% | H(0.65) → Chat, V(0.35) → Agents/Tokens/Files |
| `focus` | Chat 90% + minimal sidebar | H(0.90) → Chat, V(0.10) → Token mini |
| `review` | Chat 40% + Diff 60% | H(0.40) → Chat, Diff |
| `debug` | Chat 40% + Log 30% + Diff 30% | H(0.40) → Chat, V(0.50) → Log, Diff |
| `monitor` | Agents 40% + Tokens 30% + Log 30% | H(0.40) → Agents, V(0.50) → Tokens, Log |

```rust
/// Switch between layout presets
pub fn apply_preset(state: &mut AppState, preset: LayoutPreset) {
    state.layout_tree = match preset {
        LayoutPreset::Default => LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.65,
            min_sizes: (40, 25),
            children: Box::new((
                LayoutNode::Pane { id: PaneId::new(), pane_type: PaneType::Chat, min_size: (40, 10), visible: true, priority: PanePriority::Critical },
                LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 0.35,
                    min_sizes: (8, 8),
                    children: Box::new((
                        LayoutNode::Pane { id: PaneId::new(), pane_type: PaneType::AgentActivity, min_size: (30, 8), visible: true, priority: PanePriority::High },
                        LayoutNode::Split {
                            direction: SplitDirection::Vertical,
                            ratio: 0.50,
                            min_sizes: (6, 8),
                            children: Box::new((
                                LayoutNode::Pane { id: PaneId::new(), pane_type: PaneType::TokenDashboard, min_size: (30, 6), visible: true, priority: PanePriority::High },
                                LayoutNode::Pane { id: PaneId::new(), pane_type: PaneType::FileTree, min_size: (25, 8), visible: true, priority: PanePriority::Medium },
                            )),
                        },
                    )),
                },
            )),
        },
        LayoutPreset::Focus => { /* ... */ },
        LayoutPreset::Review => { /* ... */ },
        LayoutPreset::Debug => { /* ... */ },
        LayoutPreset::Monitor => { /* ... */ },
    };

    // Re-apply persisted scroll/fold state to new panes
    state.layout_solution = solve_layout(&state.layout_tree, state.terminal_rect());
    state.restore_persisted_state();
    state.mark_urgent();
}
```

### Automatic Layout Adaptation

The layout engine adapts dynamically based on agent activity:

```rust
/// Auto-adapt layout based on current agent state
pub fn auto_adapt(state: &mut AppState) {
    let has_diff = state.diff.has_pending_hunks();
    let has_approval = !state.approval_queue.is_empty();
    let agent_count = state.agents.active_count();
    let has_streaming = state.chat.is_streaming();

    // If approval is pending, ensure ApprovalDialog is visible
    if has_approval && !state.layout_tree.is_visible(PaneType::ApprovalDialog) {
        state.layout_tree.auto_show(PaneType::ApprovalDialog);
    }

    // If agent is editing files, switch to review layout if diff is large
    if has_diff && state.diff.hunk_count() > 5 {
        let current_preset = state.current_preset;
        if current_preset != LayoutPreset::Review {
            apply_preset(state, LayoutPreset::Review);
        }
    }

    // If multiple agents active, expand AgentActivity pane
    if agent_count > 2 {
        if let Some(split) = state.layout_tree.find_split_for_pane_type(PaneType::AgentActivity) {
            split.ratio = (split.ratio + 0.05).min(0.60);
        }
    }
}
```

## Pane Resize Constraints

### Minimum Size Enforcement

Each pane type declares minimum dimensions. The solver never allocates smaller:

```rust
const PANE_MINIMA: &[(PaneType, u16, u16)] = &[
    (PaneType::Chat,          40, 10),
    (PaneType::Diff,          50, 12),
    (PaneType::FileTree,      25, 10),
    (PaneType::AgentActivity, 30,  8),
    (PaneType::TokenDashboard, 30, 6),
    (PaneType::LogConsole,    40,  8),
    (PaneType::Timeline,      50, 10),
    (PaneType::ApprovalDialog, 50, 12),
    (PaneType::InputBar,      40,  3),
    (PaneType::StatusBar,     40,  1),
];
```

### Terminal Size Requirements

| Terminal Size | Available Panes | Fallback |
|---|---|---|
| ≥ 120×40 | All panes | Full layout |
| 100×35 | Chat + AgentActivity + TokenDashboard | Collapse FileTree |
| 80×30 | Chat + TokenDashboard | Collapse sidebar |
| 60×25 | Chat only (fullscreen) | Sidebar hidden, Ctrl+Tab overlay |
| < 60×25 | Error: terminal too small | Show min size message |

```rust
/// Graceful degradation for small terminals
pub fn solve_for_terminal_size(size: (u16, u16)) -> LayoutNode {
    match size {
        (w, h) if w >= 120 && h >= 40 => default_layout(),
        (w, h) if w >= 100 && h >= 35 => medium_layout(),
        (w, h) if w >= 80  && h >= 30 => compact_layout(),
        (w, h) if w >= 60  && h >= 25 => minimal_layout(),
        _ => panic!("Terminal too small. Minimum: 60×25. Current: {}×{}", size.0, size.1),
    }
}
```
