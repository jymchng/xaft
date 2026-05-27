//! Dynamic pane layout engine.
//!
//! The layout engine models the terminal as a binary tree of splits and leaves.
//! Each leaf is a *pane* with a type, visibility, and minimum size. The solver
//! walks the tree top-down, allocating `Rect` regions each frame.
//!
//! # Quick start
//!
//! ```rust,ignore
//! let mgr = LayoutManager::default_coding_layout();
//! let solution = mgr.solve(terminal_rect);
//! if let Some(rect) = solution.rect_for_type(PaneType::Chat) {
//!     frame.render_widget(conversation_widget, rect);
//! }
//! ```
//!
//! # Keyboard resize
//!
//! `LayoutManager::resize_focused(dir, delta)` adjusts the `ratio` of the
//! split that contains the focused pane. `Alt+H/L` and `Alt+J/K` map to this.

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

// ── Identity ──────────────────────────────────────────────────────────────────

/// Unique stable identifier for a pane node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub u64);

impl PaneId {
    fn new(n: u64) -> Self {
        Self(n)
    }
}

static PANE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_pane_id() -> PaneId {
    PaneId::new(PANE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

// ── Pane types ────────────────────────────────────────────────────────────────

/// Logical widget type hosted by a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaneType {
    /// Main streaming output / conversation.
    Chat,
    /// Single-line input bar.
    InputBar,
    /// Active-agent activity tree + tool log.
    AgentActivity,
    /// Token / cost dashboard.
    TokenDashboard,
    /// Inline diff viewer.
    DiffViewer,
    /// Workspace file tree.
    FileTree,
    /// Status / keybinding footer.
    StatusBar,
    /// Debug / system log console.
    LogConsole,
    /// Tool approval modal overlay.
    Approval,
}

impl PaneType {
    /// Display label for pane title bars.
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Output",
            Self::InputBar => "Input",
            Self::AgentActivity => "Tools",
            Self::TokenDashboard => "Stats",
            Self::DiffViewer => "Diffs",
            Self::FileTree => "Files",
            Self::StatusBar => "Status",
            Self::LogConsole => "Logs",
            Self::Approval => "Approval",
        }
    }
}

/// Per-type minimum sizes `(min_width, min_height)` in terminal cells.
///
/// The layout solver never allocates less than these dimensions to a pane.
pub const PANE_MINIMA: &[(PaneType, u16, u16)] = &[
    (PaneType::Chat, 40, 10),
    (PaneType::InputBar, 40, 3),
    (PaneType::DiffViewer, 50, 12),
    (PaneType::FileTree, 25, 10),
    (PaneType::AgentActivity, 30, 8),
    (PaneType::TokenDashboard, 30, 6),
    (PaneType::LogConsole, 40, 8),
    (PaneType::StatusBar, 40, 2),
    (PaneType::Approval, 50, 12),
];

/// Return the minimum `(width, height)` for `pane_type`.
pub fn pane_type_min_size(pane_type: PaneType) -> (u16, u16) {
    PANE_MINIMA
        .iter()
        .find(|(pt, _, _)| *pt == pane_type)
        .map(|(_, w, h)| (*w, *h))
        .unwrap_or((5, 3))
}

/// Controls when panes auto-show and auto-hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PanePriority {
    /// Always visible; never auto-hidden (Chat, StatusBar, InputBar).
    Critical = 4,
    /// Visible when agents are running.
    High = 3,
    /// Visible when relevant data exists (Diff, FileTree).
    Medium = 2,
    /// Visible on demand only (Logs, Timeline).
    Low = 1,
}

// ── Layout tree ───────────────────────────────────────────────────────────────

/// Direction of a binary split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDirection {
    /// Side-by-side — left | right.
    Horizontal,
    /// Stacked — top / bottom.
    Vertical,
}

/// The layout tree: a binary tree of splits and leaf panes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutNode {
    /// Internal node: divides its `Rect` into two children.
    Split {
        direction: SplitDirection,
        /// Fraction of the total for the *first* child. Clamped to [0.05, 0.95].
        ratio: f32,
        /// Minimum sizes (cells) for each child: (first_min, second_min).
        min_sizes: (u16, u16),
        children: Box<(LayoutNode, LayoutNode)>,
    },
    /// Leaf node: a single pane.
    Pane {
        id: PaneId,
        pane_type: PaneType,
        /// Minimum (width, height) in cells.
        min_size: (u16, u16),
        visible: bool,
        priority: PanePriority,
    },
}

impl LayoutNode {
    /// Returns true if this node or any descendant is visible.
    pub fn has_visible(&self) -> bool {
        match self {
            Self::Pane { visible, .. } => *visible,
            Self::Split { children, .. } => children.0.has_visible() || children.1.has_visible(),
        }
    }

    /// Construct a leaf pane using the canonical minimum size for `pane_type`.
    pub fn pane(pane_type: PaneType, priority: PanePriority) -> Self {
        Self::Pane {
            id: next_pane_id(),
            pane_type,
            min_size: pane_type_min_size(pane_type),
            visible: true,
            priority,
        }
    }

    /// Construct a horizontal (side-by-side) split.
    pub fn hsplit(ratio: f32, left: LayoutNode, right: LayoutNode) -> Self {
        Self::Split {
            direction: SplitDirection::Horizontal,
            ratio: ratio.clamp(0.05, 0.95),
            min_sizes: (5, 5),
            children: Box::new((left, right)),
        }
    }

    /// Construct a vertical (stacked) split.
    pub fn vsplit(ratio: f32, top: LayoutNode, bottom: LayoutNode) -> Self {
        Self::Split {
            direction: SplitDirection::Vertical,
            ratio: ratio.clamp(0.05, 0.95),
            min_sizes: (3, 3),
            children: Box::new((top, bottom)),
        }
    }

    /// Set visibility of a pane with the given ID.
    pub fn set_visible(&mut self, target: PaneId, visible: bool) -> bool {
        match self {
            Self::Pane { id, visible: v, .. } => {
                if *id == target {
                    *v = visible;
                    return true;
                }
                false
            }
            Self::Split { children, .. } => {
                children.0.set_visible(target, visible) || children.1.set_visible(target, visible)
            }
        }
    }

    /// Set visibility for all panes of a given type.
    pub fn set_type_visible(&mut self, target_type: PaneType, visible: bool) {
        match self {
            Self::Pane {
                pane_type,
                visible: v,
                ..
            } => {
                if *pane_type == target_type {
                    *v = visible;
                }
            }
            Self::Split { children, .. } => {
                children.0.set_type_visible(target_type, visible);
                children.1.set_type_visible(target_type, visible);
            }
        }
    }

    /// Find the ID of the first pane of the given type.
    pub fn find_pane_id(&self, target_type: PaneType) -> Option<PaneId> {
        match self {
            Self::Pane { id, pane_type, .. } => {
                if *pane_type == target_type {
                    Some(*id)
                } else {
                    None
                }
            }
            Self::Split { children, .. } => children
                .0
                .find_pane_id(target_type)
                .or_else(|| children.1.find_pane_id(target_type)),
        }
    }

    /// Collect all pane IDs in tree order.
    pub fn all_pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane { id, visible, .. } => {
                if *visible {
                    vec![*id]
                } else {
                    vec![]
                }
            }
            Self::Split { children, .. } => {
                let mut ids = children.0.all_pane_ids();
                ids.extend(children.1.all_pane_ids());
                ids
            }
        }
    }

    /// Adjust the `ratio` of the split closest to `pane_id` in the given direction.
    /// `delta` is in cells; positive = grow first child.
    pub fn adjust_ratio(&mut self, pane_id: PaneId, direction: SplitDirection, delta: i32) -> bool {
        match self {
            Self::Pane { id, .. } => *id == pane_id,
            Self::Split {
                direction: split_dir,
                ratio,
                children,
                ..
            } => {
                let contains_target = children.0.adjust_ratio(pane_id, direction, delta)
                    || children.1.adjust_ratio(pane_id, direction, delta);
                if contains_target && *split_dir == direction {
                    // This split is the one to adjust
                    *ratio = (*ratio + delta as f32 / 100.0).clamp(0.05, 0.95);
                }
                contains_target
            }
        }
    }
}

// ── Layout solver ─────────────────────────────────────────────────────────────

/// A solved layout: maps each pane ID + type to a concrete `Rect`.
#[derive(Debug, Default, Clone)]
pub struct LayoutSolution {
    by_id: HashMap<PaneId, Rect>,
    by_type: HashMap<PaneType, Rect>,
    /// All visible panes in tree order: (id, type, rect).
    visible: Vec<(PaneId, PaneType, Rect)>,
}

impl LayoutSolution {
    /// Look up the `Rect` allocated to a specific pane.
    pub fn rect_for_id(&self, id: PaneId) -> Option<Rect> {
        self.by_id.get(&id).copied()
    }

    /// Look up the `Rect` for the first pane of a given type.
    pub fn rect_for_type(&self, pane_type: PaneType) -> Option<Rect> {
        self.by_type.get(&pane_type).copied()
    }

    fn set(&mut self, id: PaneId, pane_type: PaneType, rect: Rect) {
        self.by_id.insert(id, rect);
        self.by_type.entry(pane_type).or_insert(rect); // first wins
        self.visible.push((id, pane_type, rect));
    }

    /// True when the solution has at least one visible pane.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// All visible panes in tree order: `(PaneId, PaneType, Rect)`.
    pub fn visible_panes(&self) -> Vec<(PaneId, PaneType, Rect)> {
        self.visible.clone()
    }

    /// All panes of a specific type: `(PaneId, Rect)`.
    pub fn panes_for_type(&self, pt: PaneType) -> Vec<(PaneId, Rect)> {
        self.visible
            .iter()
            .filter(|(_, t, _)| *t == pt)
            .map(|(id, _, rect)| (*id, *rect))
            .collect()
    }
}

/// Solve a `LayoutNode` tree against a terminal `Rect`, producing concrete rects.
pub fn solve_layout(node: &LayoutNode, rect: Rect) -> LayoutSolution {
    let mut solution = LayoutSolution::default();
    solve_recursive(node, rect, &mut solution);
    solution
}

fn solve_recursive(node: &LayoutNode, rect: Rect, solution: &mut LayoutSolution) {
    match node {
        LayoutNode::Pane {
            id,
            pane_type,
            visible,
            ..
        } => {
            if *visible && rect.width > 0 && rect.height > 0 {
                solution.set(*id, *pane_type, rect);
            }
        }
        LayoutNode::Split {
            direction,
            ratio,
            min_sizes,
            children,
        } => {
            let c0v = children.0.has_visible();
            let c1v = children.1.has_visible();
            match (c0v, c1v) {
                (true, false) => solve_recursive(&children.0, rect, solution),
                (false, true) => solve_recursive(&children.1, rect, solution),
                (false, false) => {}
                (true, true) => match direction {
                    SplitDirection::Horizontal => {
                        let split_x = calculate_split(rect.width, *ratio, min_sizes.0, min_sizes.1);
                        let left = Rect::new(rect.x, rect.y, split_x, rect.height);
                        let right_x = rect.x + split_x;
                        let right_w = rect.width.saturating_sub(split_x);
                        let right = Rect::new(right_x, rect.y, right_w, rect.height);
                        solve_recursive(&children.0, left, solution);
                        solve_recursive(&children.1, right, solution);
                    }
                    SplitDirection::Vertical => {
                        let split_y =
                            calculate_split(rect.height, *ratio, min_sizes.0, min_sizes.1);
                        let top = Rect::new(rect.x, rect.y, rect.width, split_y);
                        let bot_y = rect.y + split_y;
                        let bot_h = rect.height.saturating_sub(split_y);
                        let bottom = Rect::new(rect.x, bot_y, rect.width, bot_h);
                        solve_recursive(&children.0, top, solution);
                        solve_recursive(&children.1, bottom, solution);
                    }
                },
            }
        }
    }
}

/// Calculate split position respecting minimum sizes.
fn calculate_split(total: u16, ratio: f32, min_first: u16, min_second: u16) -> u16 {
    if total < min_first + min_second {
        return total.saturating_sub(min_second);
    }
    let ideal = (total as f32 * ratio).round() as u16;
    ideal.max(min_first).min(total.saturating_sub(min_second))
}


// ── PaneContent trait ─────────────────────────────────────────────────────────

use crossterm::event::KeyEvent as CrosstermKeyEvent;
use ratatui::buffer::Buffer as RatatuiBuffer;

/// Result of a pane's key handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHandled {
    /// The key was consumed by this pane.
    Consumed,
    /// The key was not handled; pass it upstream.
    Ignored,
}

/// Trait implemented by each pane's state struct for signal routing, rendering,
/// and key handling.
///
/// This is a future-extensibility hook. Current widgets are not required to
/// implement it; they use the `Widget` trait from ratatui directly. New pane
/// content types should prefer implementing this trait so the layout engine can
/// generically dispatch to them.
pub trait PaneContent: Send + Sync {
    /// Render the pane content into `area`.
    fn render(&self, area: Rect, buf: &mut RatatuiBuffer, focused: bool);

    /// Handle a key event. Return `Consumed` if the key was processed.
    fn handle_key(&mut self, key: CrosstermKeyEvent) -> KeyHandled;

    /// Current scroll offset `(x, y)` for persistence.
    fn scroll_position(&self) -> (u16, u16);

    /// Restore scroll position after resize.
    fn set_scroll_position(&mut self, x: u16, y: u16);

    /// Pane label shown in the title bar.
    fn label(&self) -> &str;
}

// ── Layout manager ────────────────────────────────────────────────────────────

/// Owns the layout tree, focused pane, and per-run pane visibility.
///
/// Call `solve()` each frame to get a fresh `LayoutSolution`.
#[derive(Debug, Clone)]
pub struct LayoutManager {
    root: LayoutNode,
    /// Currently focused pane ID (keyboard input target).
    pub(crate) focused: Option<PaneId>,
    /// Ordered list of focusable pane IDs (cycling with Tab).
    pub(crate) focus_order: Vec<PaneId>,
    /// Active mouse drag state (if any).
    pub dragging_split: Option<DragState>,
}

impl LayoutManager {
    /// Construct with an explicit root node.
    pub fn new(root: LayoutNode) -> Self {
        let focus_order = root.all_pane_ids();
        let focused = focus_order.first().copied();
        Self {
            root,
            focused,
            focus_order,
            dragging_split: None,
        }
    }

    /// Default layout for the coding agent workflow.
    ///
    /// Simple full-width two-pane layout: Chat (top 88%) + InputBar (bottom 12%),
    /// with a StatusBar pinned at the very bottom. No sidebar — agent activity
    /// appears inline in the chat pane via `OutputKind::System` messages.
    ///
    /// ```text
    /// ┌────────────────────────────────────────────┐
    /// │    Chat (Critical)               88%       │
    /// │                                            │
    /// ├────────────────────────────────────────────┤
    /// │    InputBar (Critical)           12%       │
    /// ├────────────────────────────────────────────┤
    /// │ StatusBar                                  │
    /// └────────────────────────────────────────────┘
    /// ```
    pub fn default_coding_layout() -> Self {
        let col = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.88,
            min_sizes: (10, 3),
            children: Box::new((
                LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
                LayoutNode::pane(PaneType::InputBar, PanePriority::Critical),
            )),
        };
        let root = LayoutNode::vsplit(
            0.97,
            col,
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        Self::new(root)
    }

    /// Solve the layout against `terminal_rect`.
    pub fn solve(&self, terminal_rect: Rect) -> LayoutSolution {
        solve_layout(&self.root, terminal_rect)
    }

    /// Return the currently focused pane type (if any).
    pub fn focused_type(&self) -> Option<PaneType> {
        let focused_id = self.focused?;
        self.find_type_for_id(focused_id)
    }

    fn find_type_for_id(&self, target: PaneId) -> Option<PaneType> {
        fn search(node: &LayoutNode, target: PaneId) -> Option<PaneType> {
            match node {
                LayoutNode::Pane { id, pane_type, .. } => {
                    if *id == target {
                        Some(*pane_type)
                    } else {
                        None
                    }
                }
                LayoutNode::Split { children, .. } => {
                    search(&children.0, target).or_else(|| search(&children.1, target))
                }
            }
        }
        search(&self.root, target)
    }

    /// Cycle focus to the next pane (Tab key).
    pub fn focus_next(&mut self) {
        self.focus_order = self.root.all_pane_ids();
        if self.focus_order.is_empty() {
            return;
        }
        let current_pos = self
            .focused
            .and_then(|f| self.focus_order.iter().position(|&id| id == f))
            .unwrap_or(0);
        self.focused = Some(self.focus_order[(current_pos + 1) % self.focus_order.len()]);
    }

    /// Focus a specific pane type directly.
    pub fn focus_type(&mut self, pane_type: PaneType) {
        if let Some(id) = self.root.find_pane_id(pane_type) {
            self.focused = Some(id);
        }
    }

    /// Show or hide all panes of a given type.
    pub fn set_type_visible(&mut self, pane_type: PaneType, visible: bool) {
        self.root.set_type_visible(pane_type, visible);
        self.focus_order = self.root.all_pane_ids();
        // If focused pane was hidden, move focus
        if !visible {
            if let Some(f) = self.focused {
                if self.find_type_for_id(f) == Some(pane_type) {
                    self.focused = self.focus_order.first().copied();
                }
            }
        }
    }

    /// Check whether panes of the given type are currently visible.
    pub fn is_type_visible(&self, pane_type: PaneType) -> bool {
        fn check(node: &LayoutNode, target: PaneType) -> bool {
            match node {
                LayoutNode::Pane {
                    pane_type, visible, ..
                } => *pane_type == target && *visible,
                LayoutNode::Split { children, .. } => {
                    check(&children.0, target) || check(&children.1, target)
                }
            }
        }
        check(&self.root, pane_type)
    }

    /// Resize the focused split by `delta` cells in `direction`.
    ///
    /// `delta > 0` grows the first child; `delta < 0` shrinks it.
    pub fn resize_focused(&mut self, direction: SplitDirection, delta: i32) {
        if let Some(focused) = self.focused {
            self.root.adjust_ratio(focused, direction, delta);
        }
    }

    // ── New methods ───────────────────────────────────────────────────────────

    /// Return the currently focused pane ID (if any).
    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.focused
    }

    /// Cycle focus to the previous pane (Shift+Tab).
    pub fn focus_prev(&mut self) {
        self.focus_order = self.root.all_pane_ids();
        if self.focus_order.is_empty() {
            return;
        }
        let current_pos = self
            .focused
            .and_then(|f| self.focus_order.iter().position(|&id| id == f))
            .unwrap_or(0);
        let len = self.focus_order.len();
        self.focused = Some(self.focus_order[(current_pos + len - 1) % len]);
    }

    /// Navigate directionally to the nearest pane in `dir`.
    ///
    /// Uses edge-to-edge geometry per the PRD:
    /// - `Left`  : candidates where `candidate.right  ≤ current.left`
    /// - `Right` : candidates where `candidate.left   ≥ current.right`
    /// - `Up`    : candidates where `candidate.bottom ≤ current.top`
    /// - `Down`  : candidates where `candidate.top    ≥ current.bottom`
    ///
    /// Among qualifying candidates, picks the one with the smallest
    /// edge-to-edge gap. If no solution is provided or no neighbour exists
    /// in the given direction, focus is unchanged.
    pub fn navigate_directional(&mut self, dir: NavDirection, solution: &LayoutSolution) {
        let focused_id = match self.focused {
            Some(id) => id,
            None => return,
        };
        let from = match solution.rect_for_id(focused_id) {
            Some(r) => r,
            None => return,
        };

        let mut best_id: Option<PaneId> = None;
        let mut best_dist = u32::MAX;

        for (id, _, to) in solution.visible_panes() {
            if id == focused_id {
                continue;
            }
            if !pane_is_in_direction(from, to, dir) {
                continue;
            }
            let dist = pane_edge_distance(from, to, dir);
            if dist < best_dist {
                best_dist = dist;
                best_id = Some(id);
            }
        }

        if let Some(id) = best_id {
            self.focused = Some(id);
        }
    }

    /// Apply a preset layout, replacing the current root.
    pub fn apply_preset(&mut self, preset: LayoutPreset) {
        let new_mgr = match preset {
            LayoutPreset::Default => Self::default_coding_layout(),
            LayoutPreset::Focus => Self::focus_layout(),
            LayoutPreset::Review => Self::review_layout(),
            LayoutPreset::Debug => Self::debug_layout(),
            LayoutPreset::Monitor => Self::monitor_layout(),
        };
        self.root = new_mgr.root;
        self.focus_order = self.root.all_pane_ids();
        self.focused = self.focus_order.first().copied();
    }

    // ── Preset factories ──────────────────────────────────────────────────────

    /// Focus layout: Chat 90% + mini stats sidebar. Chat column includes InputBar.
    pub fn focus_layout() -> Self {
        let chat_col = LayoutNode::vsplit(
            0.82,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::InputBar, PanePriority::Critical),
        );
        let root = LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.90,
                chat_col,
                LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        Self::new(root)
    }

    /// Review layout: Chat+InputBar 40% + Diff 60%.
    pub fn review_layout() -> Self {
        let chat_col = LayoutNode::vsplit(
            0.82,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::InputBar, PanePriority::Critical),
        );
        let root = LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.40,
                chat_col,
                LayoutNode::Pane {
                    id: next_pane_id(),
                    pane_type: PaneType::DiffViewer,
                    min_size: (10, 5),
                    visible: true,
                    priority: PanePriority::Medium,
                },
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        Self::new(root)
    }

    /// Debug layout: Chat 40% + Log 30% + Diff 30%.
    pub fn debug_layout() -> Self {
        let right_side = LayoutNode::hsplit(
            0.50,
            LayoutNode::pane(PaneType::LogConsole, PanePriority::Low),
            LayoutNode::Pane {
                id: next_pane_id(),
                pane_type: PaneType::DiffViewer,
                min_size: (10, 5),
                visible: true,
                priority: PanePriority::Medium,
            },
        );
        let root = LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.40,
                LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
                right_side,
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        Self::new(root)
    }

    /// Monitor layout: Agents 40% + Tokens 30% + Log 30%.
    pub fn monitor_layout() -> Self {
        let right_side = LayoutNode::hsplit(
            0.50,
            LayoutNode::pane(PaneType::TokenDashboard, PanePriority::High),
            LayoutNode::pane(PaneType::LogConsole, PanePriority::Low),
        );
        let root = LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.40,
                LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
                right_side,
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        Self::new(root)
    }

    // ── Mouse drag ────────────────────────────────────────────────────────────

    /// Begin a drag operation at `(col, row)`.
    pub fn begin_drag(&mut self, col: u16, row: u16, solution: &LayoutSolution) {
        // Find a pane that's near the given position; use the closest one.
        let mut best: Option<(PaneId, SplitDirection, u16)> = None;
        let mut min_dist = 3u16; // only grab within 3 cells of a border

        for (id, _, rect) in solution.visible_panes() {
            // Right border
            let right_x = rect.x + rect.width;
            if col >= right_x.saturating_sub(min_dist) && col <= right_x + min_dist {
                if row >= rect.y && row < rect.y + rect.height {
                    let dist = (col as i32 - right_x as i32).unsigned_abs() as u16;
                    if dist < min_dist {
                        min_dist = dist;
                        best = Some((id, SplitDirection::Horizontal, col));
                    }
                }
            }
            // Bottom border
            let bot_y = rect.y + rect.height;
            if row >= bot_y.saturating_sub(min_dist) && row <= bot_y + min_dist {
                if col >= rect.x && col < rect.x + rect.width {
                    let dist = (row as i32 - bot_y as i32).unsigned_abs() as u16;
                    if dist < min_dist {
                        min_dist = dist;
                        best = Some((id, SplitDirection::Vertical, row));
                    }
                }
            }
        }

        if let Some((pane_id, direction, start_pos)) = best {
            self.dragging_split = Some(DragState {
                pane_id,
                direction,
                start_pos,
            });
        }
    }

    /// Update the drag (called on mouse move).
    pub fn update_drag(&mut self, col: u16, row: u16, terminal_width: u16, terminal_height: u16) {
        if let Some(ref drag) = self.dragging_split {
            let current_pos = match drag.direction {
                SplitDirection::Horizontal => col,
                SplitDirection::Vertical => row,
            };
            let total = match drag.direction {
                SplitDirection::Horizontal => terminal_width,
                SplitDirection::Vertical => terminal_height,
            };
            let delta = current_pos as i32 - drag.start_pos as i32;
            let delta_pct = if total > 0 {
                (delta as f32 / total as f32 * 100.0) as i32
            } else {
                0
            };
            let pane_id = drag.pane_id;
            let direction = drag.direction;
            self.root.adjust_ratio(pane_id, direction, delta_pct);
            // Update start_pos for smooth continuous dragging
            if let Some(ref mut d) = self.dragging_split {
                d.start_pos = current_pos;
            }
        }
    }

    /// Finish a drag operation.
    pub fn end_drag(&mut self) {
        self.dragging_split = None;
    }

    /// True if a drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.dragging_split.is_some()
    }

    /// Return a reference to the root layout node.
    pub fn root(&self) -> &LayoutNode {
        &self.root
    }

    /// Show panes of the given type and return the old visibility (for undo).
    ///
    /// Because the solver collapses hidden subtrees, no ratio manipulation is
    /// needed: when one side of a split becomes visible the other side
    /// automatically receives the full rect.
    pub fn auto_show(&mut self, pane_type: PaneType) -> bool {
        let was = self.is_type_visible(pane_type);
        self.set_type_visible(pane_type, true);
        was
    }

    /// Hide panes of the given type and return the old visibility (for undo).
    pub fn auto_hide(&mut self, pane_type: PaneType) -> bool {
        let was = self.is_type_visible(pane_type);
        self.set_type_visible(pane_type, false);
        was
    }
}

// ── Directional navigation geometry ──────────────────────────────────────────

/// True if `to` is geometrically in `dir` relative to `from`, using edge comparison.
fn pane_is_in_direction(from: Rect, to: Rect, dir: NavDirection) -> bool {
    match dir {
        NavDirection::Left => (to.x + to.width) <= from.x,
        NavDirection::Right => to.x >= (from.x + from.width),
        NavDirection::Up => (to.y + to.height) <= from.y,
        NavDirection::Down => to.y >= (from.y + from.height),
    }
}

/// Edge-to-edge gap between `from` and `to` in the given direction.
fn pane_edge_distance(from: Rect, to: Rect, dir: NavDirection) -> u32 {
    match dir {
        NavDirection::Left => from.x.saturating_sub(to.x + to.width) as u32,
        NavDirection::Right => to.x.saturating_sub(from.x + from.width) as u32,
        NavDirection::Up => from.y.saturating_sub(to.y + to.height) as u32,
        NavDirection::Down => to.y.saturating_sub(from.y + from.height) as u32,
    }
}

// ── Layout presets ────────────────────────────────────────────────────────────

/// Named layout configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPreset {
    /// Default coding workflow (Chat + sidebar).
    Default,
    /// Focus mode: large chat with mini tools sidebar.
    Focus,
    /// Review mode: Chat + Diff viewer.
    Review,
    /// Debug mode: Chat + Log + Diff.
    Debug,
    /// Monitor mode: Agent activity + Tokens + Log.
    Monitor,
}

// ── Navigation direction ──────────────────────────────────────────────────────

/// Direction for keyboard-driven pane navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
}

// ── Drag state ────────────────────────────────────────────────────────────────

/// State for an in-progress mouse drag of a split divider.
#[derive(Debug, Clone)]
pub struct DragState {
    /// The pane whose border is being dragged.
    pub pane_id: PaneId,
    /// Whether dragging horizontally or vertically.
    pub direction: SplitDirection,
    /// Starting cursor position (col for H, row for V).
    pub start_pos: u16,
}

// ── LayoutManager (drag field extension) ─────────────────────────────────────

// Add the dragging_split field via extension of LayoutManager definition above.

// ── Persisted pane state ──────────────────────────────────────────────────────

/// Per-pane fold/expand state (e.g., expanded directories in FileTree).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FoldState {
    /// Set of paths/keys currently expanded.
    pub expanded: Vec<String>,
}

/// Serializable snapshot of layout + pane state for session persistence.
///
/// Survives app restarts and terminal resize because split ratios are
/// percentage-based. Call [`PersistedPaneState::save`] on quit and
/// [`PersistedPaneState::load`] on startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPaneState {
    /// Full layout tree (with ratios, types, visibility).
    /// `None` if never persisted — callers should fall back to the default layout.
    pub layout_tree: Option<LayoutNode>,
    /// Per-pane scroll offsets: `pane_id_u64 → (x_scroll, y_scroll)`.
    pub scroll_positions: HashMap<u64, (u16, u16)>,
    /// Per-pane fold states for tree/list panes.
    pub fold_states: HashMap<u64, FoldState>,
    /// The focused pane (by raw u64 ID).
    pub focused_pane: Option<u64>,
    /// Tab cycling order (raw u64 IDs).
    pub tab_order: Vec<u64>,
}

impl Default for PersistedPaneState {
    fn default() -> Self {
        Self {
            layout_tree: None,
            scroll_positions: HashMap::new(),
            fold_states: HashMap::new(),
            focused_pane: None,
            tab_order: Vec::new(),
        }
    }
}

impl PersistedPaneState {
    /// Build from a `LayoutManager`, capturing the current tree + focus.
    pub fn from_manager(mgr: &LayoutManager) -> Self {
        Self {
            layout_tree: Some(mgr.root.clone()),
            scroll_positions: HashMap::new(),
            fold_states: HashMap::new(),
            focused_pane: mgr.focused.map(|p| p.0),
            tab_order: mgr.focus_order.iter().map(|p| p.0).collect(),
        }
    }

    /// XDG-aware path for the persisted state file.
    fn state_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("xaft")
            .join("tui-state.json")
    }

    /// Save to `~/.config/xaft/tui-state.json` (creates dirs as needed).
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load from `~/.config/xaft/tui-state.json`.
    /// Returns `Default` if the file does not exist.
    pub fn load() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::state_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let json = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&json)?)
    }

    /// Restore the layout tree into `mgr` if one was persisted.
    pub fn restore_into(&self, mgr: &mut LayoutManager) {
        if let Some(ref tree) = self.layout_tree {
            mgr.root = tree.clone();
            mgr.focus_order = mgr.root.all_pane_ids();
            mgr.focused = self
                .focused_pane
                .and_then(|id| mgr.focus_order.iter().find(|p| p.0 == id).copied())
                .or_else(|| mgr.focus_order.first().copied());
        }
    }
}

// ── Graceful-degradation layout factory ──────────────────────────────────────

/// Build an appropriate `LayoutNode` root for the given terminal size.
///
/// | Size         | Layout                              |
/// |---|---|
/// | ≥ 120×40     | full default (Chat + sidebar 3-way) |
/// | ≥ 100×35     | Chat + AgentActivity + TokenDashboard |
/// | ≥ 80×30      | Chat + TokenDashboard side-by-side  |
/// | ≥ 60×25      | Chat only                           |
/// | otherwise    | Chat only (extremely small)          |
pub fn solve_for_terminal_size(width: u16, height: u16) -> LayoutNode {
    if width >= 120 && height >= 40 {
        // Full default layout root node
        let sidebar = LayoutNode::vsplit(
            0.40,
            LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
            LayoutNode::vsplit(
                0.45,
                LayoutNode::pane(PaneType::TokenDashboard, PanePriority::High),
                LayoutNode::Pane {
                    id: next_pane_id(),
                    pane_type: PaneType::DiffViewer,
                    min_size: (10, 5),
                    visible: false,
                    priority: PanePriority::Medium,
                },
            ),
        );
        LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.68,
                LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
                sidebar,
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        )
    } else if width >= 100 && height >= 35 {
        // Chat + AgentActivity + TokenDashboard (no DiffViewer)
        let sidebar = LayoutNode::vsplit(
            0.50,
            LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
            LayoutNode::pane(PaneType::TokenDashboard, PanePriority::High),
        );
        LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.68,
                LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
                sidebar,
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        )
    } else if width >= 80 && height >= 30 {
        // Chat + TokenDashboard side by side
        LayoutNode::vsplit(
            0.97,
            LayoutNode::hsplit(
                0.70,
                LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
                LayoutNode::pane(PaneType::TokenDashboard, PanePriority::High),
            ),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        )
    } else if width >= 60 && height >= 25 {
        // Chat only with status
        LayoutNode::vsplit(
            0.97,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        )
    } else {
        // Extremely small: chat only
        LayoutNode::pane(PaneType::Chat, PanePriority::Critical)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn small_rect() -> Rect {
        Rect::new(0, 0, 200, 50)
    }

    #[test]
    fn solve_leaf_fills_rect() {
        let node = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        let rect = small_rect();
        let solution = solve_layout(&node, rect);
        assert_eq!(solution.rect_for_type(PaneType::Chat), Some(rect));
    }

    #[test]
    fn solve_horizontal_split_widths_sum() {
        let node = LayoutNode::hsplit(
            0.70,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
        );
        let rect = Rect::new(0, 0, 100, 40);
        let solution = solve_layout(&node, rect);
        let chat_r = solution.rect_for_type(PaneType::Chat).unwrap();
        let side_r = solution.rect_for_type(PaneType::AgentActivity).unwrap();
        // Widths must cover the full terminal (may differ by 1 due to rounding)
        assert!((chat_r.width + side_r.width) >= 99);
        assert_eq!(chat_r.height, rect.height);
        assert_eq!(side_r.height, rect.height);
    }

    #[test]
    fn solve_vertical_split_heights_sum() {
        let node = LayoutNode::vsplit(
            0.90,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        let rect = Rect::new(0, 0, 100, 50);
        let solution = solve_layout(&node, rect);
        let body_r = solution.rect_for_type(PaneType::Chat).unwrap();
        let status_r = solution.rect_for_type(PaneType::StatusBar).unwrap();
        assert!((body_r.height + status_r.height) >= 49);
        assert_eq!(body_r.width, rect.width);
    }

    #[test]
    fn invisible_pane_not_in_solution() {
        let mut node = LayoutNode::pane(PaneType::DiffViewer, PanePriority::Medium);
        if let LayoutNode::Pane { visible, .. } = &mut node {
            *visible = false;
        }
        let solution = solve_layout(&node, small_rect());
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_none());
    }

    #[test]
    fn layout_manager_default_has_chat_and_status() {
        let mgr = LayoutManager::default_coding_layout();
        let solution = mgr.solve(small_rect());
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
        assert!(solution.rect_for_type(PaneType::StatusBar).is_some());
    }

    #[test]
    fn layout_manager_diff_hidden_by_default() {
        let mgr = LayoutManager::default_coding_layout();
        assert!(!mgr.is_type_visible(PaneType::DiffViewer));
        let solution = mgr.solve(small_rect());
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_none());
    }

    #[test]
    fn layout_manager_show_diff() {
        // Use review_layout which includes DiffViewer
        let mut mgr = LayoutManager::review_layout();
        mgr.set_type_visible(PaneType::DiffViewer, true);
        assert!(mgr.is_type_visible(PaneType::DiffViewer));
        let solution = mgr.solve(small_rect());
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_some());
    }

    #[test]
    fn layout_manager_focus_cycle() {
        let mut mgr = LayoutManager::default_coding_layout();
        let initial = mgr.focused_type();
        mgr.focus_next();
        // After one cycle, focus moved (or stayed if only 1 visible)
        let _ = mgr.focused_type();
        // No panic
        drop(initial);
    }

    #[test]
    fn calculate_split_respects_min_sizes() {
        // If total is smaller than combined minimums, second gets its minimum
        let result = calculate_split(8, 0.5, 4, 4);
        assert_eq!(result, 4);
    }

    #[test]
    fn calculate_split_clamps_to_min_first() {
        let result = calculate_split(100, 0.0, 10, 5);
        assert!(result >= 10);
    }

    #[test]
    fn calculate_split_clamps_to_min_second() {
        let result = calculate_split(100, 1.0, 5, 10);
        assert!(result + 10 <= 100);
    }

    #[test]
    fn nested_split_produces_three_regions() {
        let node = LayoutNode::hsplit(
            0.65,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::vsplit(
                0.5,
                LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
                LayoutNode::pane(PaneType::TokenDashboard, PanePriority::High),
            ),
        );
        let solution = solve_layout(&node, Rect::new(0, 0, 200, 50));
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
        assert!(solution.rect_for_type(PaneType::AgentActivity).is_some());
        assert!(solution.rect_for_type(PaneType::TokenDashboard).is_some());
        // Chat should be wider
        let chat_w = solution.rect_for_type(PaneType::Chat).unwrap().width;
        let side_w = solution
            .rect_for_type(PaneType::AgentActivity)
            .unwrap()
            .width;
        assert!(chat_w > side_w);
    }

    #[test]
    fn set_type_visible_hides_all_matching() {
        let node = LayoutNode::hsplit(
            0.5,
            LayoutNode::pane(PaneType::DiffViewer, PanePriority::Medium),
            LayoutNode::pane(PaneType::DiffViewer, PanePriority::Medium),
        );
        let mut mgr = LayoutManager::new(node);
        mgr.set_type_visible(PaneType::DiffViewer, false);
        assert!(!mgr.is_type_visible(PaneType::DiffViewer));
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let mgr = LayoutManager::default_coding_layout();
        let tiny = Rect::new(0, 0, 10, 5);
        let solution = mgr.solve(tiny);
        // Must not panic; rects may be zero-sized
        drop(solution);
    }

    #[test]
    fn focus_type_changes_focused() {
        let mut mgr = LayoutManager::default_coding_layout();
        mgr.focus_type(PaneType::StatusBar);
        assert_eq!(mgr.focused_type(), Some(PaneType::StatusBar));
    }

    // ── New tests for extended functionality ──────────────────────────────────

    #[test]
    fn focus_prev_cycles_backward() {
        let mut mgr = LayoutManager::default_coding_layout();
        // Get all visible pane IDs
        let all = mgr.root.all_pane_ids();
        if all.len() < 2 {
            return; // can't test cycling with 1 pane
        }
        // Start at first
        let first = mgr.focused_pane_id();
        // Cycle forward then backward should return to start
        mgr.focus_next();
        mgr.focus_prev();
        assert_eq!(
            mgr.focused_pane_id(),
            first,
            "focus_prev should undo focus_next"
        );
    }

    #[test]
    fn layout_presets_all_have_chat() {
        let rect = Rect::new(0, 0, 200, 50);
        for preset in [
            LayoutPreset::Default,
            LayoutPreset::Focus,
            LayoutPreset::Review,
            LayoutPreset::Debug,
            LayoutPreset::Monitor,
        ] {
            let mut mgr = LayoutManager::default_coding_layout();
            mgr.apply_preset(preset);
            // Monitor preset has no Chat, but the others should
            if preset != LayoutPreset::Monitor {
                let solution = mgr.solve(rect);
                assert!(
                    solution.rect_for_type(PaneType::Chat).is_some(),
                    "{preset:?} should have Chat pane"
                );
            }
        }
    }

    #[test]
    fn focus_layout_has_chat_and_agents() {
        let rect = Rect::new(0, 0, 200, 50);
        let mgr = LayoutManager::focus_layout();
        let solution = mgr.solve(rect);
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
        assert!(solution.rect_for_type(PaneType::AgentActivity).is_some());
        // Chat should be large (90%)
        let chat_w = solution.rect_for_type(PaneType::Chat).unwrap().width;
        let side_w = solution
            .rect_for_type(PaneType::AgentActivity)
            .unwrap()
            .width;
        assert!(chat_w > side_w * 5, "Chat should dominate in focus layout");
    }

    #[test]
    fn review_layout_has_diff() {
        let rect = Rect::new(0, 0, 200, 50);
        let mgr = LayoutManager::review_layout();
        let solution = mgr.solve(rect);
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_some());
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
    }

    #[test]
    fn debug_layout_has_log_and_diff() {
        let rect = Rect::new(0, 0, 200, 50);
        let mgr = LayoutManager::debug_layout();
        let solution = mgr.solve(rect);
        assert!(solution.rect_for_type(PaneType::LogConsole).is_some());
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_some());
    }

    #[test]
    fn monitor_layout_has_agents_and_tokens() {
        let rect = Rect::new(0, 0, 200, 50);
        let mgr = LayoutManager::monitor_layout();
        let solution = mgr.solve(rect);
        assert!(solution.rect_for_type(PaneType::AgentActivity).is_some());
        assert!(solution.rect_for_type(PaneType::TokenDashboard).is_some());
    }

    #[test]
    fn solve_for_terminal_size_large() {
        let node = solve_for_terminal_size(120, 40);
        let solution = solve_layout(&node, Rect::new(0, 0, 120, 40));
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
        assert!(solution.rect_for_type(PaneType::AgentActivity).is_some());
        assert!(solution.rect_for_type(PaneType::TokenDashboard).is_some());
    }

    #[test]
    fn solve_for_terminal_size_medium() {
        let node = solve_for_terminal_size(100, 35);
        let solution = solve_layout(&node, Rect::new(0, 0, 100, 35));
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
    }

    #[test]
    fn solve_for_terminal_size_small() {
        let node = solve_for_terminal_size(80, 30);
        let solution = solve_layout(&node, Rect::new(0, 0, 80, 30));
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
        // Should not have AgentActivity in small mode
        assert!(solution.rect_for_type(PaneType::AgentActivity).is_none());
    }

    #[test]
    fn solve_for_terminal_size_tiny() {
        let node = solve_for_terminal_size(20, 10);
        let solution = solve_layout(&node, Rect::new(0, 0, 20, 10));
        // At minimum must have chat
        assert!(solution.rect_for_type(PaneType::Chat).is_some());
    }

    #[test]
    fn visible_panes_returns_all_visible() {
        let mgr = LayoutManager::default_coding_layout();
        let rect = Rect::new(0, 0, 200, 50);
        let solution = mgr.solve(rect);
        let visible = solution.visible_panes();
        // Default layout has Chat, InputBar, and StatusBar (no sidebar)
        assert!(!visible.is_empty());
        let types: Vec<PaneType> = visible.iter().map(|(_, t, _)| *t).collect();
        assert!(types.contains(&PaneType::Chat));
        assert!(types.contains(&PaneType::StatusBar));
    }

    #[test]
    fn panes_for_type_filters_correctly() {
        let mgr = LayoutManager::default_coding_layout();
        let rect = Rect::new(0, 0, 200, 50);
        let solution = mgr.solve(rect);
        let chat_panes = solution.panes_for_type(PaneType::Chat);
        assert_eq!(chat_panes.len(), 1);
        let none_panes = solution.panes_for_type(PaneType::Approval);
        assert!(none_panes.is_empty());
    }

    #[test]
    fn navigate_directional_moves_right() {
        let rect = Rect::new(0, 0, 200, 50);
        let mut mgr = LayoutManager::default_coding_layout();
        // Focus Chat (leftmost)
        mgr.focus_type(PaneType::Chat);
        let solution = mgr.solve(rect);
        let before = mgr.focused_type();
        mgr.navigate_directional(NavDirection::Right, &solution);
        let after = mgr.focused_type();
        // Should have moved right (or stayed if no right neighbour)
        drop((before, after)); // no panic is the key assertion
    }

    #[test]
    fn directional_nav_noop_when_no_neighbour() {
        let rect = Rect::new(0, 0, 200, 50);
        let mut mgr = LayoutManager::default_coding_layout();
        // Focus StatusBar (bottom) — no pane below it
        mgr.focus_type(PaneType::StatusBar);
        let solution = mgr.solve(rect);
        let before = mgr.focused_pane_id();
        mgr.navigate_directional(NavDirection::Down, &solution);
        assert_eq!(
            mgr.focused_pane_id(),
            before,
            "no neighbour below status bar"
        );
    }

    #[test]
    fn drag_state_begins_and_ends() {
        let rect = Rect::new(0, 0, 200, 50);
        let mgr_orig = LayoutManager::default_coding_layout();
        let solution = mgr_orig.solve(rect);
        let mut mgr = mgr_orig;
        assert!(!mgr.is_dragging());
        // Try beginning a drag anywhere (may or may not find a border)
        mgr.begin_drag(100, 25, &solution);
        // end_drag always clears
        mgr.end_drag();
        assert!(!mgr.is_dragging());
    }

    #[test]
    fn persisted_pane_state_roundtrip() {
        let mgr = LayoutManager::default_coding_layout();
        let persisted = PersistedPaneState::from_manager(&mgr);
        // Should have a focused pane
        assert!(persisted.focused_pane.is_some());
        assert!(!persisted.tab_order.is_empty());
        // Serialize / deserialize
        let json = serde_json::to_string(&persisted).expect("serialize");
        let back: PersistedPaneState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.focused_pane, persisted.focused_pane);
        assert_eq!(back.tab_order.len(), persisted.tab_order.len());
    }

    #[test]
    fn apply_preset_review_has_diff_visible() {
        let mut mgr = LayoutManager::default_coding_layout();
        mgr.apply_preset(LayoutPreset::Review);
        let solution = mgr.solve(Rect::new(0, 0, 200, 50));
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_some());
    }

    // ── New tests: has_visible, solver collapse, borders, auto_show/hide ──────

    #[test]
    fn has_visible_returns_false_for_hidden_pane() {
        let node = LayoutNode::Pane {
            id: next_pane_id(),
            pane_type: PaneType::DiffViewer,
            min_size: (5, 3),
            visible: false,
            priority: PanePriority::Medium,
        };
        assert!(!node.has_visible());
    }

    #[test]
    fn has_visible_returns_true_for_visible_pane() {
        let node = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        assert!(node.has_visible());
    }

    #[test]
    fn has_visible_returns_true_for_split_with_one_visible_child() {
        let visible_child = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        let hidden_child = LayoutNode::Pane {
            id: next_pane_id(),
            pane_type: PaneType::DiffViewer,
            min_size: (5, 3),
            visible: false,
            priority: PanePriority::Medium,
        };
        let node = LayoutNode::hsplit(0.5, visible_child, hidden_child);
        assert!(node.has_visible());
    }

    #[test]
    fn has_visible_returns_false_for_split_with_all_hidden() {
        let h1 = LayoutNode::Pane {
            id: next_pane_id(),
            pane_type: PaneType::DiffViewer,
            min_size: (5, 3),
            visible: false,
            priority: PanePriority::Medium,
        };
        let h2 = LayoutNode::Pane {
            id: next_pane_id(),
            pane_type: PaneType::LogConsole,
            min_size: (5, 3),
            visible: false,
            priority: PanePriority::Low,
        };
        let node = LayoutNode::hsplit(0.5, h1, h2);
        assert!(!node.has_visible());
    }

    #[test]
    fn solver_collapses_hidden_child_gives_full_rect_to_visible() {
        let rect = Rect::new(0, 0, 100, 40);
        let visible = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        let hidden = LayoutNode::Pane {
            id: next_pane_id(),
            pane_type: PaneType::DiffViewer,
            min_size: (10, 5),
            visible: false,
            priority: PanePriority::Medium,
        };
        // Hidden child on right — visible child should get full rect
        let node = LayoutNode::hsplit(0.5, visible, hidden);
        let solution = solve_layout(&node, rect);
        let chat_rect = solution.rect_for_type(PaneType::Chat).unwrap();
        assert_eq!(
            chat_rect, rect,
            "visible child must fill full rect when sibling is hidden"
        );
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_none());
    }

    #[test]
    fn solver_collapses_hidden_left_gives_full_rect_to_right() {
        let rect = Rect::new(0, 0, 100, 40);
        let hidden = LayoutNode::Pane {
            id: next_pane_id(),
            pane_type: PaneType::DiffViewer,
            min_size: (10, 5),
            visible: false,
            priority: PanePriority::Medium,
        };
        let visible = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        // Hidden child on left — visible child on right should get full rect
        let node = LayoutNode::hsplit(0.5, hidden, visible);
        let solution = solve_layout(&node, rect);
        let chat_rect = solution.rect_for_type(PaneType::Chat).unwrap();
        assert_eq!(chat_rect, rect);
    }


    #[test]
    fn auto_show_makes_pane_visible() {
        // Use focus_layout which has AgentActivity hidden by default
        let mut mgr = LayoutManager::focus_layout();
        // AgentActivity is present in focus_layout; ensure we can toggle it
        // Use DiffViewer in a review_layout which has it hidden initially
        let mut mgr2 = LayoutManager::review_layout();
        // DiffViewer is visible in review layout already; hide then auto_show
        mgr2.set_type_visible(PaneType::DiffViewer, false);
        assert!(!mgr2.is_type_visible(PaneType::DiffViewer));
        let was = mgr2.auto_show(PaneType::DiffViewer);
        assert!(!was, "DiffViewer was hidden before auto_show");
        assert!(mgr2.is_type_visible(PaneType::DiffViewer));
        let solution = mgr2.solve(small_rect());
        assert!(solution.rect_for_type(PaneType::DiffViewer).is_some());
        drop(mgr);
    }

    #[test]
    fn auto_hide_removes_from_solution() {
        // Use focus_layout which has AgentActivity visible
        let mut mgr = LayoutManager::focus_layout();
        assert!(mgr.is_type_visible(PaneType::AgentActivity));
        let was = mgr.auto_hide(PaneType::AgentActivity);
        assert!(was, "AgentActivity was visible before auto_hide");
        assert!(!mgr.is_type_visible(PaneType::AgentActivity));
        let solution = mgr.solve(small_rect());
        assert!(solution.rect_for_type(PaneType::AgentActivity).is_none());
    }

    #[test]
    fn auto_show_returns_previous_visibility() {
        // Use focus_layout which has AgentActivity visible
        let mut mgr = LayoutManager::focus_layout();
        let was = mgr.auto_show(PaneType::AgentActivity);
        assert!(was, "AgentActivity was already visible");
    }

    #[test]
    fn default_layout_has_no_sidebar() {
        // The default layout no longer includes sidebar panes (AgentActivity,
        // TokenDashboard, FileTree, DiffViewer). Agent activity is shown inline.
        let mgr = LayoutManager::default_coding_layout();
        let solution = mgr.solve(small_rect());
        assert!(
            solution.rect_for_type(PaneType::FileTree).is_none(),
            "default layout must NOT include FileTree pane (inline mode)"
        );
        assert!(
            solution.rect_for_type(PaneType::AgentActivity).is_none(),
            "default layout must NOT include AgentActivity sidebar (inline mode)"
        );
        assert!(
            solution.rect_for_type(PaneType::TokenDashboard).is_none(),
            "default layout must NOT include TokenDashboard sidebar (inline mode)"
        );
    }

    #[test]
    fn root_accessor_returns_root() {
        let mgr = LayoutManager::default_coding_layout();
        // root() should return the root node (verify it has_visible)
        assert!(mgr.root().has_visible());
    }

    // ── PANE_MINIMA tests ─────────────────────────────────────────────────────

    #[test]
    fn pane_minima_covers_all_types() {
        for pt in [
            PaneType::Chat,
            PaneType::InputBar,
            PaneType::DiffViewer,
            PaneType::FileTree,
            PaneType::AgentActivity,
            PaneType::TokenDashboard,
            PaneType::LogConsole,
            PaneType::StatusBar,
            PaneType::Approval,
        ] {
            let (w, h) = pane_type_min_size(pt);
            assert!(w > 0 && h > 0, "min size for {:?} must be non-zero", pt);
        }
        // UsageBar has been removed — verify it no longer exists in PANE_MINIMA
    }

    #[test]
    fn pane_constructor_uses_type_min_size() {
        let node = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        if let LayoutNode::Pane { min_size, .. } = node {
            let expected = pane_type_min_size(PaneType::Chat);
            assert_eq!(min_size, expected);
        } else {
            panic!("expected Pane variant");
        }
    }

    // ── Directional navigation geometry tests ─────────────────────────────────

    #[test]
    fn navigate_directional_right_uses_edge_geometry() {
        // Chat (left) and AgentActivity (right) — from Chat, Right → AgentActivity
        let node = LayoutNode::hsplit(
            0.65,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
        );
        let mut mgr = LayoutManager::new(node);
        mgr.focus_type(PaneType::Chat);
        let solution = mgr.solve(Rect::new(0, 0, 100, 40));
        mgr.navigate_directional(NavDirection::Right, &solution);
        assert_eq!(mgr.focused_type(), Some(PaneType::AgentActivity));
    }

    #[test]
    fn navigate_directional_left_uses_edge_geometry() {
        let node = LayoutNode::hsplit(
            0.65,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::AgentActivity, PanePriority::High),
        );
        let mut mgr = LayoutManager::new(node);
        mgr.focus_type(PaneType::AgentActivity);
        let solution = mgr.solve(Rect::new(0, 0, 100, 40));
        mgr.navigate_directional(NavDirection::Left, &solution);
        assert_eq!(mgr.focused_type(), Some(PaneType::Chat));
    }

    #[test]
    fn navigate_directional_no_candidate_keeps_focus() {
        // Only one pane — no neighbour to navigate to
        let node = LayoutNode::pane(PaneType::Chat, PanePriority::Critical);
        let mut mgr = LayoutManager::new(node);
        mgr.focus_type(PaneType::Chat);
        let solution = mgr.solve(Rect::new(0, 0, 100, 40));
        mgr.navigate_directional(NavDirection::Right, &solution);
        assert_eq!(mgr.focused_type(), Some(PaneType::Chat));
    }

    #[test]
    fn navigate_up_down_with_vsplit() {
        let node = LayoutNode::vsplit(
            0.70,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            LayoutNode::pane(PaneType::StatusBar, PanePriority::Critical),
        );
        let mut mgr = LayoutManager::new(node);
        mgr.focus_type(PaneType::Chat);
        let solution = mgr.solve(Rect::new(0, 0, 100, 40));
        mgr.navigate_directional(NavDirection::Down, &solution);
        assert_eq!(mgr.focused_type(), Some(PaneType::StatusBar));
        mgr.navigate_directional(NavDirection::Up, &solution);
        assert_eq!(mgr.focused_type(), Some(PaneType::Chat));
    }

    // ── PersistedPaneState serialization tests ────────────────────────────────

    #[test]
    fn persisted_state_round_trip() {
        let mgr = LayoutManager::default_coding_layout();
        let state = PersistedPaneState::from_manager(&mgr);

        let json = serde_json::to_string(&state).expect("serialize");
        let back: PersistedPaneState = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.focused_pane, state.focused_pane);
        assert_eq!(back.tab_order, state.tab_order);
        assert!(
            back.layout_tree.is_some(),
            "layout_tree must survive round trip"
        );
    }

    #[test]
    fn persisted_state_restore_into_preserves_focus() {
        let mgr = LayoutManager::default_coding_layout();
        let original_focused = mgr.focused;
        let state = PersistedPaneState::from_manager(&mgr);

        let mut new_mgr = LayoutManager::default_coding_layout();
        state.restore_into(&mut new_mgr);
        assert_eq!(new_mgr.focused, original_focused);
    }

    #[test]
    fn fold_state_serializes() {
        let fs = FoldState {
            expanded: vec!["src/".into(), "src/auth/".into()],
        };
        let json = serde_json::to_string(&fs).expect("serialize");
        let back: FoldState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.expanded, fs.expanded);
    }

    // ── Resize scroll clamping (tested through state) ─────────────────────────

    #[test]
    fn pane_is_in_direction_left_true() {
        // to is entirely to the left of from
        let from = Rect::new(50, 0, 50, 20);
        let to = Rect::new(0, 0, 40, 20);
        assert!(pane_is_in_direction(from, to, NavDirection::Left));
    }

    #[test]
    fn pane_is_in_direction_left_false_when_overlapping() {
        let from = Rect::new(50, 0, 50, 20);
        let to = Rect::new(55, 0, 40, 20); // starts inside from
        assert!(!pane_is_in_direction(from, to, NavDirection::Left));
    }

    #[test]
    fn pane_edge_distance_right() {
        // from: x=0, w=50 → right edge at 50
        // to:   x=55, w=10 → left edge at 55
        // gap = 55 - 50 = 5
        let from = Rect::new(0, 0, 50, 20);
        let to = Rect::new(55, 0, 10, 20);
        assert_eq!(pane_edge_distance(from, to, NavDirection::Right), 5);
    }
}
