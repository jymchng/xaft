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

use ratatui::layout::Rect;

// ── Identity ──────────────────────────────────────────────────────────────────

/// Unique stable identifier for a pane node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(u64);

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Controls when panes auto-show and auto-hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Side-by-side — left | right.
    Horizontal,
    /// Stacked — top / bottom.
    Vertical,
}

/// The layout tree: a binary tree of splits and leaf panes.
#[derive(Debug, Clone)]
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
    /// Construct a leaf pane.
    pub fn pane(pane_type: PaneType, priority: PanePriority) -> Self {
        Self::Pane {
            id: next_pane_id(),
            pane_type,
            min_size: (5, 3),
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
                children.0.set_visible(target, visible)
                    || children.1.set_visible(target, visible)
            }
        }
    }

    /// Set visibility for all panes of a given type.
    pub fn set_type_visible(&mut self, target_type: PaneType, visible: bool) {
        match self {
            Self::Pane { pane_type, visible: v, .. } => {
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
    }

    /// True when the solution has at least one visible pane.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
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
            match direction {
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
                    let split_y = calculate_split(rect.height, *ratio, min_sizes.0, min_sizes.1);
                    let top = Rect::new(rect.x, rect.y, rect.width, split_y);
                    let bot_y = rect.y + split_y;
                    let bot_h = rect.height.saturating_sub(split_y);
                    let bottom = Rect::new(rect.x, bot_y, rect.width, bot_h);
                    solve_recursive(&children.0, top, solution);
                    solve_recursive(&children.1, bottom, solution);
                }
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
    ideal
        .max(min_first)
        .min(total.saturating_sub(min_second))
}

// ── Layout manager ────────────────────────────────────────────────────────────

/// Owns the layout tree, focused pane, and per-run pane visibility.
///
/// Call `solve()` each frame to get a fresh `LayoutSolution`.
#[derive(Debug, Clone)]
pub struct LayoutManager {
    root: LayoutNode,
    /// Currently focused pane ID (keyboard input target).
    focused: Option<PaneId>,
    /// Ordered list of focusable pane IDs (cycling with Tab).
    focus_order: Vec<PaneId>,
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
        }
    }

    /// Default layout for the coding agent workflow.
    ///
    /// ```text
    /// ┌─────────────────────────────────┬──────────────────┐
    /// │                                 │ AgentActivity    │
    /// │          Chat (Critical)        ├──────────────────┤
    /// │                                 │ TokenDashboard   │
    /// │                                 ├──────────────────┤
    /// │                                 │ DiffViewer       │
    /// ├─────────────────────────────────┴──────────────────┤
    /// │ StatusBar                                           │
    /// └─────────────────────────────────────────────────────┘
    /// ```
    pub fn default_coding_layout() -> Self {
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
                    visible: false, // auto-shown when diffs arrive
                    priority: PanePriority::Medium,
                },
            ),
        );

        let body = LayoutNode::hsplit(
            0.68,
            LayoutNode::pane(PaneType::Chat, PanePriority::Critical),
            sidebar,
        );

        let root = LayoutNode::vsplit(
            0.97, // 97% body, 1 line status
            body,
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
                LayoutNode::Pane { pane_type, visible, .. } => {
                    *pane_type == target && *visible
                }
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
        let mut mgr = LayoutManager::default_coding_layout();
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
        let side_w = solution.rect_for_type(PaneType::AgentActivity).unwrap().width;
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
}
