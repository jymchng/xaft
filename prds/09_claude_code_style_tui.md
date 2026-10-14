# XAFT TUI — Claude Code Visual Style

## Overview

This PRD describes the transformation of the xaft TUI from a pane-based bordered layout
into a single-column, borderless, text-first interface that matches the aesthetic and UX
of Claude Code. The goal is to make xaft feel like a native terminal AI assistant rather
than a multi-pane IDE.

## Design Principles

1. **Text is the interface.** No boxes, no borders, no chrome. All output is text in a
   scrollable stream.
2. **Single column.** Content flows vertically. No horizontal splits needed.
3. **Minimal status.** Everything the user needs lives in one status line at the bottom.
4. **Inline everything.** Tool calls, approvals, errors, and agent status appear inline
   in the conversation stream — not in floating overlays or sidebars.
5. **Neutral palette.** Mostly dim/fg gray. Accent color only for the current action.
   Error red. Success green. Nothing else.

## Target Visual Layout

```
  ◈ planner
  Breaking down: replace random.choice with secrets.choice

  ◉ coder
  ◆ read_file  src/password_generator.py
    ↳ 48 lines

  ◆ edit_file  src/password_generator.py
    ↳ 3 hunks, 6 lines changed

  I've replaced all uses of random.choice with secrets.choice and
  updated the import statement. Running tests to verify.

  ◆ bash_exec  python -m pytest tests/
    ↳ 3 passed in 0.8s

    done (5 turns)

  ◎ qa
  The changes are correct. APPROVED

    done (2 turns)

  ● planner
  ✓ QA approved

  All uses of random.choice have been replaced with secrets.choice.

────────────────────────────────────────────────────────────────
  xaft  ·  12.4k tokens  ·  $0.000  ·  [↑↓] scroll  [q] quit
```

## Current vs Target State

| Aspect          | Current                              | Target                          |
|-----------------|--------------------------------------|---------------------------------|
| Borders         | `Block::borders(Borders::ALL)`       | None — direct rendering         |
| Layout          | Chat + UsageBar + InputBar + Status  | Chat + Prompt + 1-row status    |
| Tool calls      | Transient overlay at bottom          | Inline permanent stream entries |
| Agent status    | Header row with phase icon           | Inline prefix in stream         |
| UsageBar        | Separate 1-row pane above input      | Merged into single status line  |
| Input           | Bordered pane, "Task" label          | Bare prompt line `  > ▌`        |
| Approval gates  | Floating modal overlay (kept)        | Inline `⚠ tool_name` entry added|
| Thinking        | Dim transient at bottom              | `  ⋯ thought` single dim line   |
| Spinner         | Bold braille `⣾⣽⣻⣷`                  | Sparse dots `⠄⠆⠇⠋⠙⠸⠰⠠`        |
| Status bar      | 2-row (stats + keybindings)          | 1-row: xaft · tokens · keys     |
| Colors          | Accent, warning, error, dim, fg      | fg + dim only; accent minimal   |

---

## Implementation Steps

Each step is self-contained. Run
`CARGO_TARGET_DIR=/root/rust_projects/xaft-target-local cargo test -p xaft-tui`
after each step and fix all failures before proceeding to the next.

---

### Step 1 — Collapse Layout to Three Panes

**Goal:** Remove `PaneType::UsageBar` and simplify the layout tree to Chat + InputBar +
StatusBar.

**Files to modify:**
- `crates/xaft-tui/src/layout.rs`
- `crates/xaft-tui/src/app.rs`
- `crates/xaft-tui/src/lib.rs`
- `crates/xaft-tui/src/widgets/mod.rs`
- `crates/xaft-tui/tests/tui_integration.rs`

**Instructions:**

1. In `layout.rs`, remove `UsageBar` from `PaneType` enum. Remove the entry
   `(PaneType::UsageBar, 40, 1)` from `PANE_MINIMA`. Remove the `Self::UsageBar => "Usage"`
   arm from `PaneType::label()`. Remove `UsageBar` from `pane_type_min_size()`.

2. In `layout.rs`, replace `default_coding_layout()` body with:

```rust
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
```

3. Update all other preset layout factories (`focus_layout`, `review_layout`, etc.) to
   remove any `PaneType::UsageBar` nodes.

4. In `app.rs`, remove the `UsageBarWidget` import and the `UsageBar` render block
   (the `if let Some(rect) = solution.rect_for_type(PaneType::UsageBar)` block).

5. In `lib.rs`, remove `pub use widgets::usage_bar::UsageBarWidget` and any
   `UsageBar`-related layout exports.

6. In `widgets/mod.rs`, remove `pub mod usage_bar`.

7. Delete `crates/xaft-tui/src/widgets/usage_bar.rs`.

8. In `tests/tui_integration.rs`, remove all `usage_bar_*` test functions and any
   assertion that references `PaneType::UsageBar`.

9. Fix all remaining compile errors. Run tests.

---

### Step 2 — Remove Chat Pane Header Row

**Goal:** The ConversationWidget no longer renders a header row with phase icon and label.
Content fills the entire pane area.

**File:** `crates/xaft-tui/src/widgets/conversation.rs`

1. Remove the `phase_icon`, `phase_label`, `phase_style` match block entirely.
2. Remove the `if area.height >= 1 { ... header ... }` block entirely.
3. Change `inner` to use the full pane area with 1-col left/right padding:

```rust
let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
let height = inner.height as usize;
```

4. Keep the background fill (`set_symbol(" ").set_style(self.theme.base())`).
5. The `focused: bool` field stays for API compatibility but no longer affects rendering.

Run tests. The test `renders_without_panic` should still pass.

---

### Step 3 — Borderless Input Prompt Line

**Goal:** The InputBar widget renders as a bare prompt line with no block border and the
same background color as the chat pane above it.

**File:** `crates/xaft-tui/src/widgets/input_bar.rs`

1. Remove `Block` and `Borders` from the import list.
2. Replace the background fill with `self.theme.base()`:

```rust
for y in area.top()..area.bottom() {
    for x in area.left()..area.right() {
        buf[(x, y)].set_symbol(" ").set_style(self.theme.base());
    }
}
if area.height == 0 || area.width < 4 { return; }

let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
```

3. Replace the unfocused placeholder logic with:

```rust
let hint = if self.state.phase.is_active() {
    "  · working…"
} else if self.state.task_done {
    "  · done"
} else {
    "  ·"
};
Line::from(Span::styled(hint, self.theme.dim()))
```

4. The focused state keeps `> ▌` prompt. Change `.style(input_bg)` to `.style(self.theme.base())`.

5. Remove the `input_bg` variable.

6. Remove `WorkflowPhase` import from this file if it is no longer used elsewhere in it.

Run tests. `renders_placeholder_when_unfocused` and `renders_empty_task_as_placeholder`
tests need updating to expect `"·"` instead of `"Tab"`.

---

### Step 4 — Single-Row Status Bar with Merged Stats

**Goal:** The status bar is always exactly 1 row. Token count and cost from the former
UsageBar are shown here.

**File:** `crates/xaft-tui/src/widgets/status_bar.rs`

1. Remove all `area.height >= 2` branching.
2. Remove `stats_area` / `help_area` / `Layout::default()` usage.
3. Replace the entire render body with:

```rust
fn render(self, area: Rect, buf: &mut Buffer) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_symbol(" ").set_style(self.theme.statusbar());
        }
    }
    if area.width < 20 || area.height == 0 { return; }

    let tokens_str = format_tokens(self.state.total_tokens());
    let cost_str = format!("${:.4}", self.state.total_cost_usd);
    let phase_part = if self.state.phase.is_active() {
        format!("  ·  {}", self.state.phase.label())
    } else {
        String::new()
    };
    let left = format!("  xaft  ·  {tokens_str}  ·  {cost_str}{phase_part}");

    let keys: &str = if self.state.approval_queue.has_pending() {
        "[a] approve  [r] reject  [s] skip"
    } else if self.state.layout_manager.focused_type()
        == Some(crate::layout::PaneType::InputBar)
    {
        "[Enter] send  [Esc] cancel  [q] quit"
    } else if self.state.task_done {
        "[Tab] next task  [q] quit"
    } else if self.state.phase.is_active() {
        "[↑↓] scroll  [Esc] cancel  [q] quit"
    } else {
        "[Tab] focus  [↑↓] scroll  [q] quit"
    };
    let right = format!("{keys}  ");

    Paragraph::new(Line::from(Span::styled(left, self.theme.statusbar())))
        .render(area, buf);
    let right_len = right.len() as u16;
    if right_len < area.width {
        buf.set_string(
            area.right().saturating_sub(right_len),
            area.top(),
            &right,
            self.theme.statusbar(),
        );
    }
}
```

4. Update status_bar tests to not reference `"Phase"` or two-row structure.

---

### Step 5 — Inline Tool Calls

**Goal:** Tool calls appear as permanent lines in the conversation stream instead of a
transient overlay at the bottom of the pane.

**File:** `crates/xaft-tui/src/state.rs`

1. Add `ToolCall` to `OutputKind` enum:

```rust
pub enum OutputKind {
    AgentText,
    ToolCall,    // ← new: permanent inline tool entry
    ToolResult,
    System,
    Error,
    Success,
}
```

2. In `TuiEvent::ToolStarted` handler, after updating `active_tool` and `tool_log`,
   push a permanent inline entry. Remove the `active_tool_status` assignment:

```rust
let inline_preview = input_preview(&input, 40);
self.push_output(OutputLine {
    kind: OutputKind::ToolCall,
    text: format!("  ◆ {}  {}", tool_name, inline_preview),
    agent: None,
    timestamp: Instant::now(),
});
// DO NOT set self.active_tool_status here
```

3. In `TuiEvent::ToolCompleted` handler (success path), append duration to the
   matching ToolCall line:

```rust
if success {
    let name = tool_name_for_err.unwrap_or_default();
    let dur_str = if duration_ms >= 1000.0 {
        format!("{:.1}s", duration_ms / 1000.0)
    } else {
        format!("{:.0}ms", duration_ms)
    };
    if let Some(entry) = self.output_lines.iter_mut().rev()
        .find(|l| l.kind == OutputKind::ToolCall && l.text.contains(&name))
    {
        entry.text.push_str(&format!("    ↳ {dur_str}"));
    }
}
```

**File:** `crates/xaft-tui/src/widgets/conversation.rs`

In `render_output_line`, add case:

```rust
OutputKind::ToolCall => Style::default().fg(theme.dim),
```

The `◆` icon and `↳` result are already embedded in the text string.

---

### Step 6 — Remove `active_tool_status`

**Goal:** Since tool calls are now inline (Step 5), the transient `active_tool_status`
field is obsolete.

**File:** `crates/xaft-tui/src/state.rs`

1. Remove `pub active_tool_status: Option<String>` from `AppState` struct.
2. Remove `active_tool_status: None` from `AppState::new()`.
3. Remove all assignments to `self.active_tool_status` throughout the file.
4. Remove all reads of `self.active_tool_status`.
5. Remove `active_tool_status = None` from `reset_for_new_task()`.

**File:** `crates/xaft-tui/src/widgets/conversation.rs`

1. Remove the `tool_status` variable and all code that reads
   `self.state.active_tool_status`.
2. Remove the `if let Some(status) = tool_status { ... spinner block ... }`.
3. The transient bottom area now only handles `active_agent_thinking`:

```rust
let at_bottom = self.state.output_scroll == 0;
let thinking_lines: Vec<&str> = if at_bottom {
    self.state
        .active_agent_thinking
        .as_deref()
        .map(|s| s.lines().collect::<Vec<_>>())
        .unwrap_or_default()
} else {
    vec![]
};
let thinking_rows = (thinking_lines.len() as u16).min((height as u16) / 2);
let history_height = height.saturating_sub(thinking_rows as usize);
```

Update tests that reference `active_tool_status` — remove or rewrite them.

---

### Step 7 — Inline Agent Phase Markers

**Goal:** Agent phase transitions are shown as `◈ planner`, `◉ coder`, etc. inline in
the stream. The `done (N turns)` line is indented and dim.

**File:** `crates/xaft-tui/src/state.rs`

In `TuiEvent::LlmCallStarting` handler, change the system message:

```rust
let icon = match agent_name.as_str() {
    "planner" | "summary" => "◈",
    "coder"               => "◉",
    "qa"                  => "◎",
    "fixer"               => "◌",
    _                     => "◆",
};
if agent_changed {
    self.push_output(OutputLine {
        kind: OutputKind::System,
        text: format!("{icon} {agent_name}"),
        agent: None,
        timestamp: Instant::now(),
    });
}
```

In `TuiEvent::AgentRunComplete` handler, change to:

```rust
self.push_output(OutputLine {
    kind: OutputKind::System,
    text: format!("    done ({turns} turns)"),
    agent: None,
    timestamp: Instant::now(),
});
```

(4-space indent so it reads as continuation under the agent marker above.)

---

### Step 8 — Single-Line Thinking Indicator

**Goal:** The `active_agent_thinking` transient shows exactly 1 line with `⋯` prefix.

**File:** `crates/xaft-tui/src/state.rs`

In `TuiEvent::AgentOutput` handler, change `active_agent_thinking` assignment to:

```rust
if !non_empty.is_empty() {
    let last = non_empty.last().unwrap_or(&"");
    let truncated: String = last.chars().take(100).collect();
    self.active_agent_thinking = Some(format!("  ⋯  {truncated}"));
}
```

`⋯` is U+22EF MIDLINE HORIZONTAL ELLIPSIS.

**File:** `crates/xaft-tui/src/widgets/conversation.rs`

Since `active_agent_thinking` is now always 0 or 1 lines, replace the loop:

```rust
} else if !thinking_lines.is_empty() {
    let max_w = inner.width.saturating_sub(2) as usize;
    let display: String = thinking_lines[0].chars().take(max_w).collect();
    all_lines.push(Line::from(Span::styled(
        display.to_string(),
        Style::default().fg(self.theme.dim).add_modifier(Modifier::ITALIC),
    )));
}
```

---

### Step 9 — Sparse Spinner

**Goal:** Replace bold braille spinner with a sparse ping-pong animation.

**File:** `crates/xaft-tui/src/state.rs`

```rust
pub fn spinner_char(&self) -> char {
    const FRAMES: &[char] = &[
        '⠄', '⠆', '⠇', '⠋', '⠙', '⠸', '⠰', '⠠',
        '⠀', '⠠', '⠰', '⠸', '⠙', '⠋', '⠇', '⠆',
    ];
    FRAMES[(self.tick as usize / 4) % FRAMES.len()]
}
```

Update `spinner_chars_rotate` test to assert against the new FRAMES set.

---

### Step 10 — Reduce Color Usage

**Goal:** Only `fg`, `dim`, `error`, `success` used in conversation rendering. `accent`
only for spinner, cursor, and phase icons in status bar.

**File:** `crates/xaft-tui/src/widgets/conversation.rs`

Update `render_output_line`:

```rust
let text_style = match line.kind {
    OutputKind::AgentText  => Style::default().fg(theme.fg),
    OutputKind::ToolCall   => Style::default().fg(theme.dim),
    OutputKind::ToolResult => Style::default().fg(theme.dim),
    OutputKind::System     => Style::default().fg(theme.dim),
    OutputKind::Error      => theme.error(),
    OutputKind::Success    => theme.success(),
};
```

Remove `Modifier::ITALIC` from `System` kind — italic is reserved for transient
`active_agent_thinking` only.

---

### Step 11 — Remove Split Border Infrastructure

**Goal:** `SplitBorder` and `collect_split_borders` no longer exist.

**File:** `crates/xaft-tui/src/layout.rs`

Remove:
- `pub struct SplitBorder { ... }`
- `pub fn collect_split_borders(...) -> Vec<SplitBorder>`
- `fn collect_borders_recursive(...)`

**File:** `crates/xaft-tui/src/lib.rs`

Remove:
- `pub use layout::SplitBorder`
- `pub use layout::collect_split_borders`

Remove any test referencing `SplitBorder` or `collect_split_borders`.

---

### Step 12 — Status Bar Separator Line

**Goal:** A thin `─` separator above the status bar provides the only visual boundary
in the entire TUI.

**File:** `crates/xaft-tui/src/widgets/status_bar.rs`

At the top of `render()`, draw the separator first, then overwrite with status text:

```rust
// Thin separator: dim dashes across the full width
let sep_style = Style::default().fg(theme.dim).bg(theme.statusbar_bg);
for x in area.left()..area.right() {
    buf[(x, area.top())].set_symbol("─").set_style(sep_style);
}
// Status text renders on the same row, overwriting separator in the text region
```

Because status text is rendered after the separator fill, it overwrites the `─`
characters where text appears. The separator shows only in the gaps between text
sections — producing a subtle, punctuated line effect.

---

## Acceptance Criteria

All of the following must be true after completing all steps:

1. `cargo test --workspace` passes with zero failures.
2. No `Block::borders(Borders::ALL)` call exists anywhere in `widgets/`.
3. No `SplitBorder`, `collect_split_borders`, `UsageBarWidget`, or `PaneType::UsageBar`
   symbol exists in the codebase.
4. No `active_tool_status` field exists on `AppState`.
5. `OutputKind::ToolCall` variant exists and is rendered dim.
6. Tool calls appear as permanent `  ◆ tool_name  preview    ↳ Nms` lines in
   `output_lines`, not as transient overlays.
7. `spinner_char()` cycles through `⠄⠆⠇⠋⠙⠸⠰⠠⠀⠠⠰⠸⠙⠋⠇⠆`.
8. Status bar is 1 row: `  xaft  ·  12.4k tokens  ·  $0.000  ·  [keys]`.
9. Input bar has no border. Background matches chat area. Unfocused shows `  ·`.
10. The `ApprovalWidget` modal overlay still renders correctly for High/Critical risk
    tool calls — it is NOT removed or changed by this PRD.
