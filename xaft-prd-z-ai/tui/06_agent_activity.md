# XAFT Agent Activity Visualization

## Multi-Agent Coordination in the TUI

xaft runs on the agtrs framework, which supports a coordinator/worker pattern. A
coordinator agent decomposes tasks into subtasks, delegates them to specialist workers,
and synthesizes results. The TUI must make this multi-agent orchestration visible and
comprehensible in real-time.

### The Visualization Challenge

- **Multiple agents running concurrently**: A coordinator may spawn 3-5 workers
- **Nested subagents**: Workers can themselves spawn sub-workers (up to depth 3)
- **Rapid state transitions**: Agents cycle through thinking → tool-calling → waiting → done
- **Signal volume**: 50+ state change signals per second during active orchestration
- **Task dependency graph**: Workers may depend on each other's output

## Agent Status Indicators

### Status Types

```rust
/// Agent status in the activity tree
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is waiting for its first task
    Idle,

    /// Agent is performing LLM inference (thinking/generating)
    Thinking,

    /// Agent is executing a tool call
    ToolCalling,

    /// Agent is waiting for a tool result (async tool)
    WaitingForResult,

    /// Agent is waiting for user approval
    AwaitingApproval,

    /// Agent is waiting for a subagent to complete
    WaitingForSubagent,

    /// Agent has completed its task
    Done,

    /// Agent encountered an error
    Failed,

    /// Agent was cancelled by the coordinator
    Cancelled,
}

impl AgentStatus {
    /// Visual indicator for each status
    pub fn icon(&self, unicode: bool) -> &'static str {
        if unicode {
            match self {
                Self::Idle              => "○",
                Self::Thinking          => "⏳",
                Self::ToolCalling       => "⚙",
                Self::WaitingForResult  => "◷",
                Self::AwaitingApproval  => "⚠",
                Self::WaitingForSubagent=> "⏸",
                Self::Done              => "✓",
                Self::Failed            => "✗",
                Self::Cancelled         => "⊘",
            }
        } else {
            match self {
                Self::Idle              => "o",
                Self::Thinking          => "~",
                Self::ToolCalling       => "*",
                Self::WaitingForResult  => "-",
                Self::AwaitingApproval  => "!",
                Self::WaitingForSubagent=> "|",
                Self::Done              => "v",
                Self::Failed            => "x",
                Self::Cancelled         => "/",
            }
        }
    }

    pub fn color(&self) -> Color {
        match self {
            Self::Idle              => Color::DarkGray,
            Self::Thinking          => Color::Yellow,
            Self::ToolCalling       => Color::Cyan,
            Self::WaitingForResult  => Color::Blue,
            Self::AwaitingApproval  => Color::Magenta,
            Self::WaitingForSubagent=> Color::Blue,
            Self::Done              => Color::Green,
            Self::Failed            => Color::Red,
            Self::Cancelled         => Color::DarkGray,
        }
    }

    /// Active: currently doing work (not idle, not done)
    pub fn is_active(&self) -> bool {
        matches!(self,
            Self::Thinking | Self::ToolCalling | Self::WaitingForResult |
            Self::AwaitingApproval | Self::WaitingForSubagent
        )
    }
}
```

### Agent Activity Pane

```
┌─ Agent Activity ─────────────────────────────────────────────┐
│                                                              │
│  ● Coordinator                              Thinking 2.1s    │
│    │                                                         │
│    ├─ ● file-editor-01                      ToolCall 1.8s    │
│    │    └─ ⚙ FileEditor("src/auth/token.rs")                 │
│    │                                                         │
│    ├─ ○ bash-runner-01                      Idle             │
│    │                                                         │
│    ├─ ● researcher-01                       Thinking 0.4s    │
│    │                                                         │
│    └─ ✓ verifier-01                         Done 3.2s        │
│         └─ ✓ ReadFile("src/auth/mod.rs")                     │
│                                                              │
│  4 agents │ 3 active │ 1 done │ Elapsed: 12.4s              │
└──────────────────────────────────────────────────────────────┘
```

## Task Tree Rendering

### TaskTree Data Structure

```rust
/// The task tree represents the hierarchical agent structure
#[derive(Debug, Clone)]
pub struct TaskTree {
    /// Root node (always the coordinator)
    root: TaskNode,
}

#[derive(Debug, Clone)]
pub struct TaskNode {
    /// Unique agent identifier
    pub agent_id: AgentId,

    /// Human-readable agent name
    pub name: String,

    /// Current status
    pub status: AgentStatus,

    /// Current tool call (if any)
    pub current_tool: Option<ToolCallInfo>,

    /// Child agents (subagents)
    pub children: Vec<TaskNode>,

    /// How long this agent has been in its current status
    pub status_duration: Duration,

    /// Total elapsed time since this agent was created
    pub total_duration: Duration,

    /// Number of tool calls completed
    pub tool_calls_completed: usize,

    /// Depth in the tree (0 = coordinator)
    pub depth: usize,
}

#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub tool_name: String,
    pub tool_input_summary: String,  // Truncated for display
    pub started_at: Instant,
    pub status: ToolCallStatus,
}
```

### Tree Rendering Algorithm

```rust
/// Render the task tree into ratatui Buffer
pub fn render_task_tree(tree: &TaskTree, area: Rect, buf: &mut Buffer, unicode: bool) {
    render_node(&tree.root, area, buf, 0, unicode, &mut 0);
}

fn render_node(
    node: &TaskNode,
    area: Rect,
    buf: &mut Buffer,
    depth: usize,
    unicode: bool,
    y_offset: &mut u16,
) {
    if *y_offset >= area.height { return; }

    let y = area.y + *y_offset;
    let indent = depth * 3; // 3 chars per depth level

    // Tree connector prefix
    let prefix = if depth == 0 {
        String::new()
    } else {
        let connector = if node == node.parent_last_child() { "└─" } else { "├─" };
        format!("{:indent$}{connector}", "", indent = indent - 3)
    };

    // Status icon
    let icon = node.status.icon(unicode);
    let icon_color = node.status.color();

    // Agent name
    let name_style = if node.status.is_active() {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    // Status label with duration
    let status_text = format!("{:?} {:.1}s", node.status, node.status_duration.as_secs_f64());
    let status_style = Style::default().fg(icon_color);

    // Compose the line
    let line = Line::from(vec![
        Span::styled(format!("{}{} ", prefix, icon), Style::default().fg(icon_color)),
        Span::styled(node.name.clone(), name_style),
        Span::raw("  "),
        Span::styled(status_text, status_style),
    ]);

    line.render(Rect::new(area.x, y, area.width, 1), buf);
    *y_offset += 1;

    // Render current tool call (if any, indented one more level)
    if let Some(tool) = &node.current_tool {
        if *y_offset >= area.height { return; }
        let tool_y = area.y + *y_offset;
        let tool_prefix = format!("{:indent$}└─", "", indent = (depth + 1) * 3 - 3);
        let tool_line = Line::from(vec![
            Span::styled(format!("{}⚙ ", tool_prefix), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{}({})", tool.tool_name, tool.tool_input_summary),
                Style::default().fg(Color::Gray),
            ),
        ]);
        tool_line.render(Rect::new(area.x, tool_y, area.width, 1), buf);
        *y_offset += 1;
    }

    // Render children
    for child in &node.children {
        render_node(child, area, buf, depth + 1, unicode, y_offset);
    }
}
```

## Subagent Nesting Display

### Deep Nesting: Up to 3 Levels

```
● Coordinator                                 Thinking 4.2s
  │
  ├─ ● task-planner-01                        ToolCall 2.1s
  │    └─ ⚙ Bash("find . -name '*.rs'")
  │
  ├─ ● code-writer-01                         Thinking 0.8s
  │    │
  │    ├─ ● file-editor-01                    AwaitingApproval 1.2s
  │    │    └─ ⚙ FileEditor("src/auth/token.rs")
  │    │
  │    └─ ● file-editor-02                    ToolCall 0.5s
  │         └─ ⚙ WriteFile("src/auth/refresh.rs")
  │
  └─ ✓ test-runner-01                         Done 8.1s
       └─ ✓ Bash("cargo test")
```

### Nesting Depth Indicators

For very deep nesting, xaft uses visual depth cues:

```rust
/// Depth-based styling
fn depth_style(depth: usize) -> Style {
    match depth {
        0 => Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        1 => Style::default().fg(Color::Cyan),
        2 => Style::default().fg(Color::Blue),
        3 => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::DarkGray),
    }
}

/// Depth-based tree connectors
fn depth_connector(depth: usize) -> &'static str {
    match depth {
        0 => "",
        1 => "├─",
        2 => "│  ├─",
        3 => "│  │  ├─",
        _ => "│  │  │  ├─",
    }
}
```

### Collapsing Deep Trees

When the tree is too tall for the pane, xaft collapses subtrees:

```
● Coordinator                                 Thinking 4.2s
  ├─ ● task-planner-01    [▶ 2 tools]         ToolCall 2.1s
  ├─ ● code-writer-01                         Thinking 0.8s
  │    ├─ ● file-editor-01                    AwaitingApproval
  │    └─ ● file-editor-02    [▶ 1 tool]      ToolCall 0.5s
  └─ ✓ test-runner-01      [▶ 1 tool]         Done 8.1s
```

The `[▶ N tools]` indicator shows collapsed child tool calls. Press Enter on a
collapsed node to expand it.

```rust
/// Collapse/expand logic
pub struct TreeCollapseState {
    /// Set of node IDs that are collapsed
    collapsed: HashSet<AgentId>,

    /// Auto-collapse threshold: collapse when children > N
    auto_collapse_threshold: usize,
}

impl TreeCollapseState {
    /// Auto-collapse subtrees that exceed the threshold
    pub fn auto_collapse(&mut self, tree: &TaskTree, pane_height: u16) {
        // If the rendered tree would exceed the pane height, collapse
        // the subtrees with the most children first
        let rendered_height = tree.rendered_height();
        if rendered_height > pane_height as usize {
            let nodes_by_children = tree.nodes_sorted_by_child_count_desc();
            for node in nodes_by_children {
                if node.children.len() > self.auto_collapse_threshold {
                    self.collapsed.insert(node.agent_id);
                }
                let new_height = tree.rendered_height_with_collapsed(&self.collapsed);
                if new_height <= pane_height as usize { break; }
            }
        }
    }
}
```

## Coordinator/Worker Activity

### Coordinator Status Display

The coordinator has a special status display showing its delegation strategy:

```
┌─ Coordinator ────────────────────────────────────────────────┐
│                                                              │
│  Phase: DELEGATING                                           │
│  Plan:  5 subtasks identified                                │
│                                                              │
│  ┌─ Delegation Plan ─────────────────────────────────────┐  │
│  │ 1. ✓ Analyze codebase structure     → researcher-01   │  │
│  │ 2. ● Fix auth token bug             → file-editor-01  │  │
│  │ 3. ○ Write unit tests               → test-writer-01  │  │
│  │ 4. ○ Update documentation           → doc-writer-01   │  │
│  │ 5. ○ Run integration tests          → test-runner-01  │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                              │
│  Progress: ████████░░░░  2/5 complete                        │
│  Strategy: sequential (dependency chain)                      │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

### Coordinator Phases

```rust
/// Coordinator delegation phases
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoordinatorPhase {
    /// Analyzing the task and creating a plan
    Planning,

    /// Delegating subtasks to workers
    Delegating,

    /// Waiting for all workers to complete
    Synthesizing,

    /// Reviewing and integrating worker results
    Reviewing,

    /// Coordinator has finished
    Complete,
}

impl CoordinatorPhase {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Planning     => "PLANNING",
            Self::Delegating   => "DELEGATING",
            Self::Synthesizing => "SYNTHESIZING",
            Self::Reviewing    => "REVIEWING",
            Self::Complete     => "COMPLETE",
        }
    }
}
```

### Worker Specialization Labels

```rust
/// Worker agent types with specialization labels
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkerType {
    FileEditor,
    BashRunner,
    Researcher,
    TestWriter,
    DocWriter,
    CodeReviewer,
    Generic,
}

impl WorkerType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::FileEditor   => "file-editor",
            Self::BashRunner   => "bash-runner",
            Self::Researcher   => "researcher",
            Self::TestWriter   => "test-writer",
            Self::DocWriter    => "doc-writer",
            Self::CodeReviewer => "code-reviewer",
            Self::Generic      => "worker",
        }
    }

    pub fn icon(&self, unicode: bool) -> &'static str {
        if unicode {
            match self {
                Self::FileEditor   => "✎",
                Self::BashRunner   => "▶",
                Self::Researcher   => "🔍",
                Self::TestWriter   => "🧪",
                Self::DocWriter    => "📄",
                Self::CodeReviewer => "👁",
                Self::Generic      => "●",
            }
        } else {
            match self {
                Self::FileEditor   => "[E]",
                Self::BashRunner   => "[>]",
                Self::Researcher   => "[R]",
                Self::TestWriter   => "[T]",
                Self::DocWriter    => "[D]",
                Self::CodeReviewer => "[C]",
                Self::Generic      => "[*]",
            }
        }
    }
}
```

## Signal-Driven Updates

### Signal → Tree Update Mapping

The TaskTree is updated exclusively through agtrs signals:

```
agtrs Signal                    →  TaskTree Update
───────────────────────────────────────────────────────────────────
TaskCreated { parent, child }   →  Add child node under parent
TaskStarted { agent_id }        →  Set status = Thinking
ToolCallStart { agent_id, tool }→  Set status = ToolCalling, set current_tool
ToolCallComplete { agent_id }   →  Clear current_tool, increment tool_calls_completed
ApprovalRequired { agent_id }   →  Set status = AwaitingApproval
ApprovalResolved { agent_id }   →  Set status = Thinking (resume)
TaskCompleted { agent_id }      →  Set status = Done
TaskFailed { agent_id, error }  →  Set status = Failed
TaskCancelled { agent_id }      →  Set status = Cancelled
CoordinatorDelegated { child }  →  Add child to coordinator's children
SubtaskCompleted { child }      →  Mark child as Done, update progress
```

### Signal Handler Implementation

```rust
/// Handle agent signals to update the task tree
impl TaskTree {
    pub fn handle_signal(&mut self, signal: &AppSignal) {
        match signal {
            AppSignal::TaskCreated { parent_id, child_id, name, worker_type } => {
                let child = TaskNode {
                    agent_id: *child_id,
                    name: name.clone(),
                    status: AgentStatus::Idle,
                    current_tool: None,
                    children: Vec::new(),
                    status_duration: Duration::ZERO,
                    total_duration: Duration::ZERO,
                    tool_calls_completed: 0,
                    depth: 0, // Will be set when inserted
                };

                if let Some(parent) = self.find_node_mut(parent_id) {
                    child.depth = parent.depth + 1;
                    parent.children.push(child);
                }
            }

            AppSignal::TaskStarted { agent_id } => {
                if let Some(node) = self.find_node_mut(agent_id) {
                    node.status = AgentStatus::Thinking;
                    node.status_duration = Duration::ZERO;
                }
            }

            AppSignal::ToolCallStart { agent_id, tool_name, tool_input } => {
                if let Some(node) = self.find_node_mut(agent_id) {
                    node.status = AgentStatus::ToolCalling;
                    node.current_tool = Some(ToolCallInfo {
                        tool_name: tool_name.clone(),
                        tool_input_summary: truncate_with_ellipsis(tool_input, 40),
                        started_at: Instant::now(),
                        status: ToolCallStatus::Running,
                    });
                }
            }

            AppSignal::ToolCallComplete { agent_id, .. } => {
                if let Some(node) = self.find_node_mut(agent_id) {
                    node.status = AgentStatus::Thinking;
                    node.current_tool = None;
                    node.tool_calls_completed += 1;
                }
            }

            AppSignal::TaskCompleted { agent_id } => {
                if let Some(node) = self.find_node_mut(agent_id) {
                    node.status = AgentStatus::Done;
                    node.current_tool = None;
                }
            }

            AppSignal::TaskFailed { agent_id, error } => {
                if let Some(node) = self.find_node_mut(agent_id) {
                    node.status = AgentStatus::Failed;
                    node.current_tool = None;
                }
            }

            _ => {}
        }
    }

    /// Find a node by agent_id (depth-first search)
    fn find_node_mut(&mut self, id: &AgentId) -> Option<&mut TaskNode> {
        self.root.find_mut(id)
    }
}

impl TaskNode {
    fn find_mut(&mut self, id: &AgentId) -> Option<&mut TaskNode> {
        if self.agent_id == *id { return Some(self); }
        for child in &mut self.children {
            if let Some(found) = child.find_mut(id) { return Some(found); }
        }
        None
    }
}
```

### Duration Tracking

A background tokio task updates `status_duration` every 100ms:

```rust
/// Update durations in the task tree (called every 100ms)
pub async fn duration_updater(state: Arc<RwLock<AppState>>) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        interval.tick().await;
        let mut s = state.write().await;
        s.agents.update_durations(Duration::from_millis(100));
        s.mark_dirty_if_active();
    }
}

impl TaskTree {
    pub fn update_durations(&mut self, delta: Duration) {
        self.root.update_durations_recursive(delta);
    }
}

impl TaskNode {
    fn update_durations_recursive(&mut self, delta: Duration) {
        if self.status.is_active() {
            self.status_duration += delta;
        }
        self.total_duration += delta;
        for child in &mut self.children {
            child.update_durations_recursive(delta);
        }
    }
}
```

## Timeline View of Agent Events

### Visual Timeline

```
┌─ Timeline ───────────────────────────────────────────────────┐
│                                                              │
│  0s    2s    4s    6s    8s    10s   12s   14s   16s        │
│  │     │     │     │     │     │     │     │     │           │
│  ├─────●═══════════●─────✓────────────────────────── Coord   │
│  │     │           │                                           │
│  │     ├───────────●═══════════⚙═══════════⚠────── file-ed   │
│  │     │                                               01     │
│  │     ├───────●═══════════●═══════════✓────────────── bash   │
│  │     │                                               01     │
│  │     └───────────●════════════════●═════✓─────────── rese   │
│  │                                                     arch   │
│  │                                                     01     │
│  │                                                           │
│  ● = Started  ⚙ = Tool call  ⚠ = Approval  ✓ = Done        │
│  ═ = Thinking/Active  ─ = Idle                              │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

### Timeline Data Structure

```rust
/// Timeline of agent events
#[derive(Debug, Clone)]
pub struct Timeline {
    /// Start time of the session
    start_time: Instant,

    /// Events in chronological order
    events: Vec<TimelineEvent>,
}

#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub timestamp: Duration,   // Time since session start
    pub agent_id: AgentId,
    pub event_type: TimelineEventType,
}

#[derive(Debug, Clone)]
pub enum TimelineEventType {
    AgentStarted,
    AgentThinking,
    ToolCallStarted { tool: String },
    ToolCallCompleted,
    ApprovalRequested,
    ApprovalResolved { approved: bool },
    AgentCompleted,
    AgentFailed { error: String },
}
```

### Timeline Rendering

```rust
/// Render the timeline view
impl Timeline {
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.events.is_empty() {
            let msg = Line::from(Span::styled(
                " No agent events yet",
                Style::default().fg(Color::DarkGray),
            ));
            msg.render(area, buf);
            return;
        }

        // Calculate time range
        let total_duration = self.events.last()
            .map(|e| e.timestamp)
            .unwrap_or(Duration::ZERO);

        // Group events by agent
        let agent_lanes: HashMap<AgentId, Vec<&TimelineEvent>> = self.events.iter()
            .filter(|e| matches!(e.event_type, TimelineEventType::AgentStarted))
            .fold(HashMap::new(), |mut acc, e| {
                acc.entry(e.agent_id).or_default();
                acc
            });

        // Render time axis
        let time_axis_y = area.y;
        render_time_axis(area, total_duration, time_axis_y, buf);

        // Render each agent lane
        let mut lane_y = time_axis_y + 2;
        for (agent_id, events) in &agent_lanes {
            render_agent_lane(*agent_id, events, area, lane_y, total_duration, buf);
            lane_y += 1;
        }
    }
}

fn render_time_axis(area: Rect, duration: Duration, y: u16, buf: &mut Buffer) {
    let secs = duration.as_secs();
    let tick_interval = match secs {
        0..=5 => 1,
        6..=30 => 5,
        31..=120 => 10,
        121..=600 => 30,
        _ => 60,
    };

    for t in (0..=secs).step_by(tick_interval as usize) {
        let x = area.x + (t as f32 / secs.max(1) as f32 * (area.width as f32 - 10.0)) as u16;
        let label = format!("{}s", t);
        let span = Span::styled(label, Style::default().fg(Color::DarkGray));
        span.render(Rect::new(x, y, label.len() as u16, 1), buf);

        // Tick mark
        buf.get_mut(x, y + 1).set_char('│').set_style(Style::default().fg(Color::DarkGray));
    }
}
```

### Swimlane Rendering

Each agent gets a horizontal swimlane. Activities are drawn as colored bars:

```rust
fn render_agent_lane(
    agent_id: AgentId,
    events: &[&TimelineEvent],
    area: Rect,
    y: u16,
    total_duration: Duration,
    buf: &mut Buffer,
) {
    // Agent label (left side)
    let name = format!("{:<10}", agent_id.short_name());
    let label = Span::styled(name, Style::default().fg(Color::Gray));
    label.render(Rect::new(area.x, y, 10, 1), buf);

    // Activity bar
    let bar_x = area.x + 11;
    let bar_width = area.width.saturating_sub(12);

    // Draw baseline (idle)
    for x in bar_x..bar_x + bar_width {
        buf.get_mut(x, y).set_char('─').set_style(Style::default().fg(Color::DarkGray));
    }

    // Overlay activity segments
    for event in events {
        let x = bar_x + (event.timestamp.as_secs_f32() / total_duration.as_secs_f32().max(0.001)
            * bar_width as f32) as u16;
        if x >= bar_x + bar_width { continue; }

        let (ch, style) = match &event.event_type {
            TimelineEventType::AgentStarted => ('●', Style::default().fg(Color::Green)),
            TimelineEventType::AgentThinking => ('═', Style::default().fg(Color::Yellow)),
            TimelineEventType::ToolCallStarted { .. } => ('⚙', Style::default().fg(Color::Cyan)),
            TimelineEventType::ApprovalRequested => ('⚠', Style::default().fg(Color::Magenta)),
            TimelineEventType::AgentCompleted => ('✓', Style::default().fg(Color::Green)),
            TimelineEventType::AgentFailed { .. } => ('✗', Style::default().fg(Color::Red)),
            _ => continue,
        };

        buf.get_mut(x, y).set_char(ch).set_style(style);
    }
}
```

## Activity Summary Bar

The bottom of the AgentActivity pane shows a summary:

```rust
/// Render activity summary bar
fn render_activity_summary(tree: &TaskTree, area: Rect, buf: &mut Buffer) {
    let stats = tree.collect_stats();

    let parts = vec![
        Span::styled(format!(" {} agents", stats.total), Style::default().fg(Color::White)),
        Span::raw(" │ "),
        Span::styled(format!("{} active", stats.active), Style::default().fg(Color::Yellow)),
        Span::raw(" │ "),
        Span::styled(format!("{} done", stats.done), Style::default().fg(Color::Green)),
        Span::raw(" │ "),
        Span::styled(format!("{} failed", stats.failed), Style::default().fg(
            if stats.failed > 0 { Color::Red } else { Color::DarkGray }
        )),
        Span::raw(" │ "),
        Span::styled(
            format!("Elapsed: {:.1}s", stats.elapsed.as_secs_f64()),
            Style::default().fg(Color::Gray),
        ),
    ];

    Line::from(parts).render(area, buf);
}

#[derive(Debug)]
struct TreeStats {
    total: usize,
    active: usize,
    done: usize,
    failed: usize,
    cancelled: usize,
    idle: usize,
    elapsed: Duration,
}

impl TaskTree {
    fn collect_stats(&self) -> TreeStats {
        let mut stats = TreeStats {
            total: 0, active: 0, done: 0, failed: 0,
            cancelled: 0, idle: 0, elapsed: Duration::ZERO,
        };
        self.root.collect_stats_recursive(&mut stats);
        stats
    }
}
```
