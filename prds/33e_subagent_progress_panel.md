# PRD 33e — Subagent / Thread Progress Panel

## Status: Draft
## Depends on: PRD 09, PRD 10, PRD 23 (ExploreAgentPool), PRD 33d (background execution)

---

## 1. Problem

When xaft runs its multi-agent pipeline (Planner → Coder → QA ↔ Fixer)
or an `ExploreAgentPool` (PRD 23), the user has no visibility into which
agents are running, how long they've been running, or how many subagents
are active. The TUI transcript shows output line-by-line but gives no
structural overview.

From the Claude Code output pasted by the user:

```
● main                                           ↑/↓ to select · Enter to view
( ) Explore  Production-readiness audit of lauren-middlewares           1m 27s
```

Claude Code renders a compact **thread/agent panel** (right-side or
bottom overlay) showing:

- The main "foreground" agent (`● main`) with its current phase.
- Named subagents with description + elapsed time.
- Navigation (`↑/↓`) and drill-down (`Enter`) to view a subagent's output.

xaft's `AgentTracker` tracks agent state internally but doesn't expose
a navigable TUI panel. Users can't tell whether the explore pool has
finished or how many files remain.

---

## 2. Goals

1. A compact **agent panel** is rendered at the top or bottom of the TUI
   showing:
   - The foreground pipeline's current agent and phase.
   - Each active/recently-completed subagent with its description and
     elapsed time.
2. `↑/↓` navigates the panel entries.
3. `Enter` on a subagent entry switches the transcript view to show that
   subagent's output (drill-down).
4. `Escape` returns to the main transcript.
5. The panel auto-dismisses when all subagents complete (after a 3-second
   grace period so the user can see the final state).
6. The panel is toggleable with `Ctrl+P` (chosen to avoid conflict with
   Emacs `Ctrl+A` / PRD 43 and `Ctrl+B` background / PRD 33d).
7. Real-time elapsed time ticks at 1-second granularity via the 10fps
   ephemeral refresh cycle.

---

## 3. Architectural Constraint: append-only renderer

Like PRD 33b, the core challenge is that xaft uses an append-only
stdout renderer. A "fixed overlay at the bottom" in a traditional
terminal UI requires either:

1. **Alternate screen** (Ratatui-style full redraws) — out of scope.
2. **Re-paint the bottom N rows each tick** using cursor movement.

**Chosen approach**: the panel is drawn in the **prompt region** as an
extension of the ephemeral layer, not as part of the commit-once transcript.
The ephemeral layer already refreshes at 10fps using cursor-up + re-draw.
The agent panel is rendered as additional rows above the input bar within
the ephemeral refresh cycle.

This means the panel:
- Never scrolls into terminal history.
- Always occupies the bottom of the visible screen.
- Updates in real time (elapsed time, status icons) because the ephemeral
  layer redraws it every 100ms.
- Does not interfere with the committed transcript.

### 3.1 Ephemeral refresh extension

Currently the ephemeral layer draws N rows above the input bar
(hint line, cost display, etc.) and then the input bar draws itself.
The agent panel slots in between: transcript → [agent panel] → [ephemeral
hints] → [input bar].

`EphemeralState` gains:
```rust
pub agent_panel: Option<AgentPanelSnapshot>,
```

`build_ephemeral(state)` populates `agent_panel` from
`state.agent_panel.snapshot()` if the panel is visible and non-empty.

`IncrementalRenderer::draw_ephemeral_block` draws the panel rows before
the hint line, factoring them into its cursor-up offset calculation.

---

## 4. Design

### 4.1 `AgentPanelEntry`

```rust
#[derive(Debug, Clone)]
pub struct AgentPanelEntry {
    pub id:           String,      // unique within session, e.g. "explore-3"
    pub label:        String,      // short name shown in panel
    pub description:  String,      // task description or file path
    pub status:       AgentPanelStatus,
    pub started_at:   Instant,
    pub finished_at:  Option<Instant>,
    /// The pipeline id this entry belongs to (for drill-down routing).
    pub pipeline_id:  u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPanelStatus {
    Running,
    Completed,
    Failed,
}

impl AgentPanelEntry {
    pub fn elapsed(&self) -> Duration {
        self.finished_at.unwrap_or_else(Instant::now) - self.started_at
    }

    pub fn elapsed_label(&self) -> String {
        let s = self.elapsed().as_secs();
        if s < 60 { format!("{s}s") } else { format!("{}m {}s", s / 60, s % 60) }
    }

    pub fn status_icon(&self) -> &'static str {
        match self.status {
            AgentPanelStatus::Running   => "○",  // spinning in live render
            AgentPanelStatus::Completed => "✓",
            AgentPanelStatus::Failed    => "✗",
        }
    }
}
```

### 4.2 `AgentPanel` widget

```rust
pub struct AgentPanel {
    /// All entries for the current session (oldest first).
    pub entries:  Vec<AgentPanelEntry>,
    /// Selected entry index for keyboard navigation.
    pub selected: Option<usize>,
    /// Whether the panel is visible.
    pub visible:  bool,
    /// If Some, the transcript is filtered to this agent's output (drill-down).
    pub drilldown_id: Option<String>,
    /// Tick for spinner animation: cycles 0..7 at render time.
    pub spinner_tick: u8,
    /// Grace-period countdown after all agents complete (in 10fps ticks).
    pub hide_countdown: Option<u8>,  // 30 ticks = 3 seconds
}

impl AgentPanel {
    /// Maximum rows the panel will consume.
    pub const MAX_ROWS: usize = 8;

    /// Entries to display (scroll window: always shows `selected` in view).
    pub fn visible_entries(&self) -> &[AgentPanelEntry] {
        let n = self.entries.len();
        if n <= Self::MAX_ROWS {
            return &self.entries;
        }
        let sel = self.selected.unwrap_or(n - 1);
        let start = sel.saturating_sub(Self::MAX_ROWS - 1);
        &self.entries[start..(start + Self::MAX_ROWS).min(n)]
    }

    pub fn spinner_char(&self) -> char {
        "⠋⠙⠹⠸⠼⠴⠦⠧".chars().nth(self.spinner_tick as usize % 8).unwrap()
    }
}
```

Stored in `AppState::agent_panel: AgentPanel`.

### 4.3 `AgentPanelSnapshot` (renderer-facing)

```rust
#[derive(Debug, Clone)]
pub struct AgentPanelEntry {
    pub label:        String,
    pub description:  String,
    pub status_icon:  String,
    pub elapsed:      String,
    pub is_selected:  bool,
}

#[derive(Debug, Clone)]
pub struct AgentPanelSnapshot {
    pub rows:        Vec<AgentPanelEntry>,
    pub drilldown:   Option<String>,   // agent id when in drill-down mode
    pub footer_hint: String,           // "↑/↓ navigate · Enter view · Esc return · Ctrl+P hide"
}
```

`AgentPanel::snapshot()` creates this from the current state. The renderer
never holds a reference to `AppState`.

### 4.4 Rendering in ephemeral block

The ephemeral block renders the panel rows above the status hints:

```
○ planner    Planning: fix the type error in src/rate_limit.rs     12s
⠙ explore-1  src/rate_limit.py                                      8s   ← selected
○ explore-2  src/cors.py                                             8s
✓ explore-4  src/timeout.py                                    done 11s
─────────────────────────────────────────────────────────────────────────
↑/↓ navigate · Enter to view · Esc return · Ctrl+P hide
❯ _
```

Each entry line format:
```
{icon} {label:<12}  {description truncated to fit}  {elapsed:>6}
```

The selected entry is highlighted (bold or `▶` prefix depending on theme).

Running entries show the spinner char cycling from `AgentPanel::spinner_char()`.

Panel separator line: `─` repeated across full terminal width, dimmed.

### 4.5 Population from signals

`AppState::handle_event` maps signals to panel updates:

| Event | Panel action |
|-------|-------------|
| `TuiEvent::AgentStarted { agent_id, role, description, pipeline_id }` | Push `AgentPanelEntry { status: Running, started_at: Instant::now(), … }`; `panel.visible = true` |
| `TuiEvent::AgentCompleted { agent_id }` | Update entry → `Completed`; `finished_at = Some(now)` |
| `TuiEvent::AgentFailed { agent_id, .. }` | Update entry → `Failed`; `finished_at = Some(now)` |
| Handoff (orchestrator advances to next agent) | `XaftAgentHandoffRequested` closes current agent entry; new entry added on next `AgentStarted` |
| All entries complete | Start `hide_countdown = Some(30)` |
| `hide_countdown` reaches 0 (tick handler) | `panel.visible = false`; clear entries |

The `ExploreAgentPool` emits per-subagent `XaftAgentStarted` /
`XaftAgentFinished` for each file-exploration worker.

### 4.6 Keyboard handling

Panel keyboard bindings are **active only when `panel.visible`**. They
are checked before the slash-palette routing in `AppState::handle_key_event`:

```rust
if self.agent_panel.visible {
    match key.code {
        KeyCode::Up => {
            self.agent_panel.select_prev();
            return InputAction::NoOp;
        }
        KeyCode::Down => {
            self.agent_panel.select_next();
            return InputAction::NoOp;
        }
        KeyCode::Enter if self.agent_panel.selected.is_some() => {
            let id = self.agent_panel.selected_entry_id();
            self.agent_panel.drilldown_id = id;
            return InputAction::NoOp;
        }
        KeyCode::Esc if self.agent_panel.drilldown_id.is_some() => {
            self.agent_panel.drilldown_id = None;
            return InputAction::NoOp;
        }
        _ => {}  // fall through to normal input handling
    }
}
```

**`Ctrl+P` toggle**:
```rust
KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
    self.agent_panel.visible = !self.agent_panel.visible;
    InputAction::NoOp
}
```

**Conflict check**: `Ctrl+P` does not appear in Emacs bindings (Emacs uses
`Ctrl+P` for previous-history, but that is handled in the input bar layer
before the panel layer). PRD 43 notes that `Ctrl+P`/`Ctrl+N` navigate
history only when the input bar is not empty and the cursor is at the
bottom — so the panel check (which fires first) takes priority only when
the panel is visible.

### 4.7 Drill-down view

When `drilldown_id = Some(agent_id)`:

1. `IncrementalRenderer` receives a `PromptState` / `EphemeralState` that
   includes a `drilldown_agent_id: Option<String>`.
2. But the committed transcript cannot be re-filtered — lines are already
   written. Instead, drill-down **replays** the buffered lines for that
   agent from `AppState::per_agent_buffers`.

```rust
/// Per-agent buffer: committed transcript lines tagged to each agent.
pub per_agent_buffers: HashMap<String, Vec<StyledLine>>,
```

When a `CommitLine` mutation fires and `agent_panel` has an active agent,
`AppState` also appends the line to `per_agent_buffers[current_agent_id]`.

On drill-down selection:
1. Emit `RenderMutation::ClearTranscript`.
2. Emit `RenderMutation::CommitLine` for each line in
   `per_agent_buffers[agent_id]`.
3. Update ephemeral to show `[viewing: explore-2]  Esc to return`.

On `Esc` (exit drill-down):
1. Emit `RenderMutation::ClearTranscript`.
2. Replay the full `per_agent_buffers` for the main agent (or all agents
   if no pipeline is active) in chronological order.

**Memory cap**: `per_agent_buffers` per-agent entry is capped at 500 lines.

### 4.8 Spinner animation

The 10fps ephemeral refresh tick also increments `panel.spinner_tick`:
```rust
self.agent_panel.spinner_tick = self.agent_panel.spinner_tick.wrapping_add(1);
```
This causes running entries to cycle through `⠋⠙⠹⠸⠼⠴⠦⠧` every ~100ms.

### 4.9 Panel visibility rules summary

| Condition | Panel state |
|-----------|-------------|
| No subagents ever started | Hidden |
| Any agent running | Visible, auto-show |
| All agents completed, < 3 s ago | Visible (countdown) |
| All agents completed, ≥ 3 s ago | Hidden |
| `Ctrl+P` pressed | Toggle regardless of above |
| Zero entries | Hidden (panel not shown even if `visible = true`) |

---

## 5. New Signals

```rust
/// Emitted when any agent (including subagents) starts running.
#[derive(Debug, Clone)]
pub struct XaftAgentStarted {
    pub agent_id:    String,
    pub role:        String,
    pub description: String,
    pub pipeline_id: u64,
}

/// Emitted when any agent finishes (success or error).
#[derive(Debug, Clone)]
pub struct XaftAgentFinished {
    pub agent_id:    String,
    pub success:     bool,
    pub duration_ms: f64,
}
```

These complement the existing `XaftAgentHandoffRequested` signal.

The `EventBridge` maps:
- `XaftAgentStarted` → `TuiEvent::AgentStarted { agent_id, role, description, pipeline_id }`
- `XaftAgentFinished` → `TuiEvent::AgentCompleted { agent_id }` or `TuiEvent::AgentFailed { agent_id, error }`

---

## 6. Interaction with Other PRDs

| PRD | Interaction |
|-----|-------------|
| PRD 33d (Background) | Background pipeline agents still emit `XaftAgentStarted`. They appear in the panel with `(bg)` prefix on their label. The panel covers both foreground and background agents. |
| PRD 33b (Collapsible output) | Drill-down replay uses the same `StyledLine` buffer that collapsible groups are built from. A drill-down replay respects the existing `CollapseGroup` state. |
| PRD 37 (/agents slash command) | `/agents` shows a static snapshot; the panel shows live state. They share the same `AgentPanelEntry` data source but are separate views. |
| PRD 43 (Vim/Emacs) | Panel `↑/↓` intercepts before vim normal mode. `Ctrl+P` does not conflict with any Vim binding. |

---

## 7. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/state.rs` | `agent_panel: AgentPanel`, `per_agent_buffers`, signal handlers, `Ctrl+P`, drill-down |
| `crates/xaft-tui/src/ephemeral.rs` | `agent_panel` field in `EphemeralState`; render panel rows |
| `crates/xaft-tui/src/renderer.rs` | Draw panel in ephemeral block; account for panel rows in cursor-up offset |
| `crates/xaft-tui/src/transcript.rs` | `agent_id` optional tag on `StyledLine` for buffer routing |
| `crates/xaft-agent/src/signals.rs` | `XaftAgentStarted`, `XaftAgentFinished` |
| `crates/xaft-tui/src/bridge.rs` | Map new signals → `TuiEvent` |
| `crates/xaft-tui/tests/agent_panel_integration.rs` | New tests |

---

## 8. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | Panel appears when any subagent is spawned, showing its label and elapsed time. |
| AC2 | Panel updates in real-time: elapsed time increments each second; spinner cycles at ~10fps. |
| AC3 | `↑/↓` navigates panel entries; selected entry is highlighted. |
| AC4 | `Enter` on an entry clears the transcript and replays that agent's buffered output. |
| AC5 | `Escape` from drill-down replays the full main-transcript buffer. |
| AC6 | Panel auto-hides 3 seconds after all agents complete. |
| AC7 | `Ctrl+P` toggles panel visibility manually regardless of agent activity. |
| AC8 | `XaftAgentStarted` and `XaftAgentFinished` signals are emitted for all agents including explore-pool subagents. |
| AC9 | Background pipeline agents appear in the panel with `(bg)` label prefix. |
| AC10 | Panel occupies at most 8 rows + 1 separator; scrolls internally if more than 8 entries. |

---

## 9. Tests

```rust
#[tokio::test]
async fn panel_appears_when_subagent_starts() {
    // Emit XaftAgentStarted.
    // Assert agent_panel.visible == true, entries.len() == 1.
}

#[tokio::test]
async fn panel_shows_elapsed_time() {
    // Emit AgentStarted, wait 1.1s, read snapshot.
    // Assert elapsed_label contains "1s".
}

#[tokio::test]
async fn panel_entry_becomes_completed_on_finish() {
    // Emit AgentStarted then AgentFinished.
    // Assert entry.status == Completed.
}

#[tokio::test]
async fn panel_auto_hides_after_countdown() {
    // All entries complete.
    // Advance 30 ephemeral ticks.
    // Assert panel.visible == false.
}

#[tokio::test]
async fn drill_down_replays_agent_buffer() {
    // Buffer 5 lines for agent "explore-2".
    // Press Enter on explore-2 entry.
    // Assert ClearTranscript + 5 CommitLine mutations emitted.
}

#[tokio::test]
async fn drill_down_esc_replays_full_transcript() {
    // Drill down, then Esc.
    // Assert ClearTranscript + full buffer replay mutations.
}

#[tokio::test]
async fn ctrl_p_toggles_panel_visibility() {
    // Emit AgentStarted (panel becomes visible).
    // Press Ctrl+P — panel.visible = false.
    // Press Ctrl+P again — panel.visible = true.
}

#[tokio::test]
async fn panel_caps_at_max_rows_visible() {
    // Emit 12 AgentStarted events.
    // Assert visible_entries().len() == AgentPanel::MAX_ROWS.
}

#[tokio::test]
async fn spinner_advances_on_tick() {
    let mut panel = AgentPanel::default();
    let tick0 = panel.spinner_char();
    panel.spinner_tick = panel.spinner_tick.wrapping_add(1);
    let tick1 = panel.spinner_char();
    assert_ne!(tick0, tick1);
}
```
