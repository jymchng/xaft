# PRD 33b — Collapsible Tool Output Sections (Ctrl+O)

## Status: Draft
## Depends on: PRD 09, PRD 10 (TUI architecture)

---

## 1. Problem

When an xaft agent runs on a non-trivial task it may invoke dozens of
tools (file reads, grep searches, git operations). The current TUI renders
every tool call and its full output as committed `StyledLine`s in the
transcript. A 40-tool run produces hundreds of lines, drowning the agent's
actual analysis and plan in raw tool I/O.

From the Claude Code output pasted by the user:

```
… +35 tool uses (ctrl+o to expand)
```

Claude Code collapses tool use bursts into a single summary line,
keeping the transcript clean. The user can press `Ctrl+O` to expand
and read the details.

Without this, xaft's transcript becomes unreadable on long tasks and
users lose the signal in the noise.

---

## 2. Goals

1. Tool calls issued between two agent text segments are grouped into
   a **collapsible section**.
2. The collapsed state shows a single summary line:
   `  ⋯  N tool uses  (Ctrl+O to expand)`.
3. Pressing `Ctrl+O` while the cursor is on or near a collapsed section
   expands it in-place, revealing the full tool call/output lines.
4. `Ctrl+O` on an already-expanded section collapses it back.
5. New tool calls while an agent is actively running are shown expanded
   (live visibility); they collapse automatically when the next agent
   text segment arrives.
6. The collapsed/expanded state is per-section and per-session (does not
   persist across sessions).

---

## 3. Non-Goals

- Collapsing agent text output (only tool calls).
- Persistent collapse state across sessions.
- Nested collapsing (sections within sections).
- In-place terminal overwrite of already-scrolled-off lines (see §4.2
  for the architectural constraint and chosen approach).

---

## 4. Architecture

### 4.1 Architectural constraint: append-only renderer

xaft uses an append-only transcript model: `IncrementalRenderer` writes
to stdout sequentially, and completed lines scroll into terminal history.
This is intentional (no Ratatui alternate screen, no full-frame redraws).

**True in-place toggle** — rewriting a collapsed line to expanded lines
after it has scrolled above the fold — is not possible without a full-screen
re-render or alternate-screen mode, both of which are out of scope.

**Chosen approach**: the toggle only operates on groups that are within
the **visible screen region** (i.e., have not scrolled off). When
`Ctrl+O` is pressed:

1. If the focused group is still on screen, erase from that group's
   start position to the current cursor, re-render the group in the new
   state, then re-render everything below it, and move the cursor back.
2. If the group has scrolled off, `Ctrl+O` is a no-op for that group
   (show a status-bar message: "Group scrolled off — cannot toggle").

This is implemented by tracking the **physical row offset** of each
group at the time it was drawn, and comparing it against `term_rows -
prompt_rows` to determine if it is still on screen.

### 4.2 `CollapseGroup` in `transcript.rs`

```rust
/// A collapsible group of tool-call lines.
#[derive(Debug, Clone)]
pub struct CollapseGroup {
    pub id:            u64,             // monotonic, assigned at creation
    pub summary:       String,          // e.g. "12 tool uses"
    pub tool_names:    Vec<String>,     // tool names in order (for summary)
    pub lines:         Vec<StyledLine>, // full content when expanded
    pub expanded:      bool,            // current display state
    pub open:          bool,            // true while still receiving lines
    /// Physical row where this group was rendered (from top of terminal).
    /// `None` while group is still open (not yet committed).
    pub rendered_row:  Option<u16>,
}

impl CollapseGroup {
    /// Human-readable summary: "3 tool uses  (read_file, grep_search, list_dir)"
    pub fn build_summary(&self) -> String {
        let n = self.lines.iter()
            .filter(|l| matches!(l.kind, LineKind::ToolCall))
            .count();
        let names: Vec<_> = self.tool_names.iter()
            .take(3)
            .map(String::as_str)
            .collect();
        let suffix = if self.tool_names.len() > 3 {
            format!(", +{}", self.tool_names.len() - 3)
        } else {
            String::new()
        };
        format!("{n} tool use{}  ({}{})",
            if n == 1 { "" } else { "s" },
            names.join(", "),
            suffix)
    }
}
```

### 4.3 `RenderMutation` additions

```rust
pub enum RenderMutation {
    // ... existing variants ...

    /// Open a new collapse group. Subsequent `CommitLine` calls with
    /// `LineKind::ToolCall` or `LineKind::ToolOutput` go into it.
    OpenCollapseGroup { id: u64 },

    /// Close the current group: compute summary, render collapsed summary
    /// line, record `rendered_row`.
    CloseCollapseGroup { id: u64 },

    /// Toggle expanded ↔ collapsed for a group that is still on screen.
    ToggleCollapseGroup { id: u64 },

    /// A tool call started within the current open group (for summary metadata).
    RecordToolName { group_id: u64, tool_name: String },
}
```

### 4.4 `IncrementalRenderer` state

```rust
pub struct IncrementalRenderer {
    // ... existing fields ...
    /// All groups committed this session, by id.
    groups:         HashMap<u64, CollapseGroup>,
    /// The currently-open (not yet committed) group id, if any.
    open_group:     Option<u64>,
    /// Next group id to assign.
    next_group_id:  u64,
    /// Physical rows written since terminal clear, used to compute rendered_row.
    rows_written:   u16,
    /// Terminal height (set from `term_rows`).
    term_rows:      u16,
}
```

**`apply(OpenCollapseGroup { id })`**:
- Assert `open_group.is_none()`.
- Create `CollapseGroup { id, expanded: true, open: true, … }`.
- Insert into `groups`.
- Set `open_group = Some(id)`.

**`apply(CommitLine(line))`** when `open_group.is_some()`:
- If line is `LineKind::ToolCall` or `LineKind::ToolOutput`:
  - Append to `groups[open_group].lines`.
  - If `open_group.expanded = true`, render the line immediately
    (live mode) with `│ ` prefix.
- Other line kinds: render normally (agent text — triggers
  auto-close of any open group first).

**`apply(RecordToolName { group_id, tool_name })`**:
- Append to `groups[group_id].tool_names`.

**`apply(CloseCollapseGroup { id })`**:
- Set `groups[id].open = false`.
- If `groups[id].lines.len() >= collapse_threshold`:
  - If already expanded (live mode): erase the rendered tool lines
    (cursor-up + erase) and write the collapsed summary line.
  - Set `groups[id].expanded = false`.
  - Record `groups[id].rendered_row = rows_written`.
- Else (below threshold): leave lines as-is (no collapse).

**`apply(ToggleCollapseGroup { id })`**:
- Look up `groups[id]`.
- Check if `rendered_row` is still on screen:
  `let on_screen = rows_written - group.rendered_row < term_rows - PROMPT_ROWS`.
- If not on screen: push `StatusMessage("Group scrolled off — cannot toggle")`.
  Return.
- If `expanded = false` (currently collapsed): erase summary line,
  write all inner lines with `│ ` prefix, set `expanded = true`.
- If `expanded = true` (currently expanded): erase all inner lines,
  write summary line, set `expanded = false`.
- In both cases update `rows_written` by the delta.

### 4.5 `AppState` grouping logic

`AppState` knows the boundary rule: **agent text → [tool burst] → agent text**.

In `AppState::handle_event`:

```rust
TuiEvent::ToolStarted { name, input, .. } => {
    if self.open_group_id.is_none() {
        let id = self.next_group_id();
        self.open_group_id = Some(id);
        self.mutations.push(RenderMutation::OpenCollapseGroup { id });
    }
    let gid = self.open_group_id.unwrap();
    self.mutations.push(RenderMutation::RecordToolName {
        group_id: gid,
        tool_name: name.clone(),
    });
    // Render the live tool-call header line into the open group.
    self.mutations.push(RenderMutation::CommitLine(
        StyledLine::new(format!("● {name}({input_summary})"), LineKind::ToolCall)
    ));
}

TuiEvent::ToolFinished { output, .. } => {
    self.mutations.push(RenderMutation::CommitLine(
        StyledLine::new(format!("  ↳ {}", truncate(&output, 120)), LineKind::ToolOutput)
    ));
}

TuiEvent::AgentOutputChunk { .. } | TuiEvent::AgentFinished { .. } => {
    // Close any open group before committing agent text.
    if let Some(id) = self.open_group_id.take() {
        self.mutations.push(RenderMutation::CloseCollapseGroup { id });
    }
    // … commit agent text as normal …
}
```

### 4.6 `Ctrl+O` keyboard binding

```rust
KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
    match self.focused_group_id {
        Some(id) => {
            self.mutations.push(RenderMutation::ToggleCollapseGroup { id });
        }
        None => {
            // No group in focus — find most-recently-closed group.
            if let Some(id) = self.last_closed_group_id {
                self.mutations.push(RenderMutation::ToggleCollapseGroup { id });
            }
        }
    }
    InputAction::NoOp
}
```

`focused_group_id` is updated on scroll: when the user scrolls up into
a region that contains a group's `rendered_row`, `focused_group_id` is
set to that group's id. Implementation: maintain `last_closed_group_id`
(simplest useful heuristic) for the common case of pressing `Ctrl+O`
immediately after the tool burst.

### 4.7 Visual format

**Collapsed** (rendered as one line):
```
  ⋯  12 tool uses  (read_file, grep_search, list_dir, +2)   [Ctrl+O to expand]
```
Style: `LineKind::System`, dimmed, `⋯` in theme accent colour.

**Expanded**:
```
  ╶  12 tool uses  [Ctrl+O to collapse]
  │  ● read_file(src/rate_limit.rs)
  │    ↳ 142 lines read
  │  ● grep_search("consume")
  │    ↳ 4 matches in src/rate_limit.rs
  │  ● list_dir(src/)
  │    ↳ 8 entries
  …
```
Inner lines prefixed with `│ ` (dim grey); tool name in white; output
in dimmed text.

### 4.8 Interaction with `ClearTranscript` (PRD 35)

When `ClearTranscript` fires, all `CollapseGroup` state is reset:
`groups.clear()`, `open_group = None`, `rows_written = 0`.

### 4.9 Live tool calls remain expanded

While `CollapseGroup::open = true`, lines are written immediately
(expanded) so the user sees live tool progress. The switch to collapsed
only happens in `CloseCollapseGroup` via cursor-up erasure.

---

## 5. `XaftConfig` opt-out

```toml
[tui]
collapse_tool_output = true   # default: true
collapse_threshold   = 3      # only collapse if ≥ N tool calls in group
```

`collapse_threshold = 1` collapses even single-tool bursts.
`collapse_threshold = 0` or `collapse_tool_output = false` disables
the feature entirely: all tool lines are always rendered inline with no
grouping.

---

## 6. Edge Cases

| Case | Behaviour |
|------|-----------|
| Group still open at session end | `TuiApp` sends `CloseCollapseGroup` before shutdown |
| Tool burst below `collapse_threshold` | Lines committed normally, no summary line |
| Consecutive tool bursts (no agent text between them) | Merged into same group |
| `Ctrl+O` with no groups | No-op |
| `Ctrl+O` on a group already scrolled off | Status bar: "Group scrolled off — cannot toggle" |
| `ClearTranscript` while group is open | Reset all group state; close open group |
| `collapse_tool_output = false` | Routing: `OpenCollapseGroup` is a no-op; lines committed directly |

---

## 7. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/transcript.rs` | `CollapseGroup`, updated `RenderMutation` |
| `crates/xaft-tui/src/renderer.rs` | Group state, `apply(Open/Close/Toggle)`, row tracking |
| `crates/xaft-tui/src/state.rs` | Grouping logic, `Ctrl+O` handler, `open_group_id`, `focused_group_id` |
| `crates/xaft-config/src/types.rs` | `TuiConfig::collapse_tool_output`, `collapse_threshold` |
| `crates/xaft-tui/tests/collapse_integration.rs` | New integration tests |

---

## 8. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | After a run with ≥ `collapse_threshold` tool calls, the transcript shows `⋯ N tool uses (Ctrl+O to expand)`. |
| AC2 | Pressing `Ctrl+O` expands the group with full tool output lines prefixed `│ `. |
| AC3 | Pressing `Ctrl+O` again collapses it back to the summary line. |
| AC4 | While the agent is actively calling tools, the group is rendered expanded (live). |
| AC5 | A group collapses automatically when the next agent text chunk arrives. |
| AC6 | `collapse_tool_output = false` renders all tool output inline with no collapse. |
| AC7 | Group count in summary line matches the actual number of tool calls. |
| AC8 | Groups below `collapse_threshold` are never collapsed (lines committed inline). |
| AC9 | `Ctrl+O` on a group that has scrolled off shows a status-bar message, not an error. |

---

## 9. Tests

`crates/xaft-tui/tests/collapse_integration.rs`:

```rust
#[tokio::test]
async fn tool_burst_is_collapsed_after_agent_text() {
    // Emit 5 ToolStarted/ToolFinished pairs, then AgentOutputChunk.
    // Assert transcript contains exactly one summary StyledLine.
}

#[tokio::test]
async fn ctrl_o_expands_group() {
    // Build state with a collapsed group at known rendered_row.
    // Press Ctrl+O.
    // Assert group.expanded == true, inner lines rendered.
}

#[tokio::test]
async fn ctrl_o_collapses_expanded_group() {
    // Expand group, press Ctrl+O again.
    // Assert group.expanded == false, only summary line rendered.
}

#[tokio::test]
async fn live_tools_rendered_expanded() {
    // While group is open, assert lines written immediately.
}

#[tokio::test]
async fn collapse_disabled_by_config_renders_inline() {
    // collapse_tool_output = false.
    // Assert no CollapseGroup created.
}

#[tokio::test]
async fn below_threshold_not_collapsed() {
    // collapse_threshold = 5, emit 3 tool calls.
    // Assert no summary line.
}

#[tokio::test]
async fn clear_transcript_resets_group_state() {
    // Open group, then ClearTranscript.
    // Assert open_group_id = None, groups = empty.
}
```
