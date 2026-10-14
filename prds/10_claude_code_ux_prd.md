# xaft TUI — Claude Code UX Emulation PRD

## Overview

This PRD describes the changes needed to make xaft's TUI feel and behave like
Claude Code's terminal interface. The goal is a single-column, text-first,
bottom-anchored conversation stream that feels native to power-users who work
in terminals all day.

Reference interface (Claude Code):

```
● Looking at the architecture: the diff viewer needs inline display (Claude
  Code style) triggered by edit_file/write_file tool completions.
  Reading 1 file… (ctrl+o to expand)
  ⎿  /root/rust_projects/xaft/crates/xaft-tui/src/state.rs
· Envisioning… (2m 31s · ↑ 8.6k tokens)
  ⎿  Tip: Use /btw to ask a quick side question
──────────────────────────────────── fix-streaming-tool-call-demux ──>
────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle);        98% context used
```

---

## Section 1 — Bottom-Anchored Scrollable Chat Pane

### Current Behaviour

- `output_lines: VecDeque<OutputLine>` is a fixed-capacity circular buffer
  (MAX 2000 lines)
- `visible_output_wrapped(height, width)` selects the last `height` visual
  rows by walking backwards from the newest entry
- When `output_scroll = 0` the newest content shows at the bottom
- When `output_scroll > 0` the user has scrolled up (older content visible)
- Padding is added at the TOP of `all_lines` so content sticks to the bottom
- `output_auto_scroll = true` by default; set to `false` when user scrolls up

### Problems

1. **Hard cap at 2000 lines** — long tasks lose early output
2. **Scroll is in logical-line units** — one press of `↑` skips 3 logical
   lines, which may be many visual rows if lines wrap
3. **No scroll-to-line-number or search** — no way to jump to a specific
   agent turn
4. **`Resize` always snaps to bottom** — loses scroll position on terminal
   resize (intentional but jarring)
5. **Wrap-aware selection is approximate** — `UnicodeWidthStr::width` counts
   bytes not display columns for some emoji/CJK ranges

### Target Behaviour

Every new message (AgentText, ToolCall, System, diff block, etc.) must:

1. Append at the **logical bottom** of `output_lines`
2. Be **immediately visible** without user interaction — the visible area
   stays pinned to the bottom unless the user has scrolled up
3. Be **scrollable upward indefinitely** — the user can press `↑` or
   `PageUp` as many times as needed to reach the earliest output
4. The cap may remain (2000 default) but should be configurable

### Implementation Instructions

#### 1.1 — Keep bottom-anchor padding (already done)

`conversation.rs` already pads `all_lines` at the top:
```rust
if content_rows < height {
    let pad = height - content_rows;
    let mut padded = vec![Line::default(); pad];
    padded.extend(all_lines.into_iter());
    all_lines = padded;
}
```
Keep this unchanged.

#### 1.2 — Scroll step granularity

Change `Up` / `Down` scroll step from 3 logical lines to **1 visual row** by
computing how many logical lines correspond to 1 visual row at the current
pane width.  Practical approach: always scroll by 1 logical line per key
press (current: 3); keep `PageUp`/`PageDown` at 15.

In `state.rs` `handle_key`:

```rust
KeyCode::Up | KeyCode::Char('k') => {
    let max = self.output_lines.len().saturating_sub(1);
    self.output_scroll = (self.output_scroll + 1).min(max);  // was +3
    self.output_auto_scroll = false;
}
KeyCode::Down | KeyCode::Char('j') => {
    self.output_scroll = self.output_scroll.saturating_sub(1);  // was -3
    if self.output_scroll == 0 {
        self.output_auto_scroll = true;
    }
}
```

#### 1.3 — Resize preserves scroll position when user scrolled up

In the `TuiEvent::Resize` handler, only snap to bottom when
`output_auto_scroll = true`. When `false`, clamp but don't snap:

```rust
TuiEvent::Resize(w, h) => {
    self.terminal_size = (w, h);
    if self.output_auto_scroll {
        self.output_scroll = 0;
    } else {
        // Clamp to valid range without snapping to bottom
        let max = self.output_lines.len().saturating_sub(1);
        self.output_scroll = self.output_scroll.min(max);
    }
}
```

#### 1.4 — Scroll indicator with line count

In `conversation.rs`, improve the scroll indicator to show how far from
the bottom the user is:

```rust
if self.state.output_scroll > 0 {
    let n = self.state.output_lines.len();
    let pct = (self.state.output_scroll * 100) / n.max(1);
    let indicator = format!(" ↑ {}% ", pct);
    // ...render at top-right of inner area
}
```

---

## Section 2 — Inline File Read Preview (Claude Code "Reading N files…")

### Target

When `read_file` or `list_files` starts, show a transient inline indicator:

```
  ◆ ReadFile(src/main.py)
    Reading 1 file… ↳ 48 lines  [3ms]
```

The "Reading 1 file…" text should appear as a transient `active_agent_thinking`
update (already shown at the bottom) WHILE the tool is in flight, then be
replaced by the permanent `  ◆ ReadFile(src/main.py)  [3ms]` line when done.

### Implementation

In `state.rs` `ToolStarted` handler, for read-only tools, set
`active_agent_thinking` to a live indicator:

```rust
if matches!(tool_name.as_str(), "read_file" | "list_files" | "grep") {
    self.active_agent_thinking =
        Some(format!("  Reading {}…", filename_from_input(&input)));
}
```

On `ToolCompleted`, the existing logic already clears `active_agent_thinking`
(via `active_tool_status` being gone). This gives the "flash" effect.

---

## Section 3 — Elapsed Time and Token Count Inline

### Target

Claude Code shows elapsed time and token cost inline in the stream:

```
· Envisioning… (2m 31s · ↑ 8.6k tokens)
```

### Implementation

The `active_agent_thinking` single-line transient already shows while an
agent is thinking. Extend it to include elapsed time and token estimate:

In the `Tick` handler (runs at 60fps), update the thinking indicator if
`stream.is_active`:

```rust
if self.stream.is_active && !self.current_agent.is_empty() {
    let elapsed = self.agent_start_time
        .map(|t| t.elapsed())
        .unwrap_or_default();
    let elapsed_str = format_elapsed(elapsed);
    let tok_str = format_tokens(self.total_tokens());
    self.active_agent_thinking = Some(format!(
        "  ⋯  {} ({elapsed_str} · ↑ {tok_str})",
        self.stream.agent_name
    ));
}
```

Add `agent_start_time: Option<Instant>` to `AppState`. Set it in
`LlmCallStarting`, clear in `AgentRunComplete`.

---

## Section 4 — Context Usage Indicator in Status Bar

### Target

```
  ⏵⏵ bypass permissions on (shift+tab to cycle);        98% context used
```

The right side of the status bar shows `N% context used` when token usage
is approaching the model's context window.

### Implementation

In `status_bar.rs`, compute `context_pct` from `total_tokens / max_context`:

```rust
// Approximate: poolside/laguna-m.1 has 262k token context
const APPROX_CONTEXT_TOKENS: u64 = 262_112;
let pct = (state.total_tokens() * 100) / APPROX_CONTEXT_TOKENS;
let ctx_str = if pct >= 70 {
    format!("  {}% context used", pct)
} else {
    String::new()
};
```

Show in red when > 90%, orange when > 70%, gray otherwise.

---

## Section 5 — Collapsible Tool Output (ctrl+o to expand)

### Target

Long tool outputs (e.g. read_file returning 500 lines) show as:
```
  ◆ ReadFile(src/huge_file.py)  [12ms]
    Reading 1 file… (ctrl+o to expand)
    ⎿  src/huge_file.py
```

User presses `Ctrl+O` to toggle expansion of the collapsed output.

### Implementation

Add a `collapsed_outputs: HashMap<String, bool>` to `AppState` keyed by
`tool_use_id`. When `read_file` returns output longer than 20 lines, push:

1. A summary line: `    ⎿  filename  (N lines, ctrl+o to expand)`
2. N detail lines marked with a `tool_use_id` association

In `conversation.rs`, when rendering an `OutputKind::ToolResult` line that
has an associated `tool_use_id` in `collapsed_outputs`, skip rendering the
detail lines if collapsed.

`Ctrl+O` in `handle_key` toggles the entry in `collapsed_outputs`.

---

## Section 6 — Git Branch in Status Bar Right-Aligned

### Target

```
──────────────────────────────── fix-streaming-tool-call-demux ──>
```

The branch name is shown right-aligned with a separator line above the
status bar. The current implementation shows the branch in the status bar
text; this should be moved to a visual divider.

### Implementation

In `status_bar.rs`, draw the separator line with the branch name embedded:

```rust
let branch = state.git_branch.as_deref().unwrap_or("");
if !branch.is_empty() && area.width > (branch.len() + 6) as u16 {
    let pad = area.width as usize - branch.len() - 5;
    let sep = format!("{} {} ──>", "─".repeat(pad), branch);
    buf.set_string(area.left(), area.top(), &sep,
        Style::default().fg(theme.dim).bg(theme.statusbar_bg));
} else {
    // plain separator
    for x in area.left()..area.right() {
        buf[(x, area.top())].set_symbol("─").set_style(sep_style);
    }
}
```

---

## Section 7 — Approval Gate as Inline Text (not modal)

### Target

Claude Code shows permission requests inline:
```
  ⏵⏵ bypass permissions on (shift+tab to cycle);
```

Rather than a floating modal, the approval request appears as an inline
message at the bottom of the chat stream with key hints embedded.

### Implementation

When `ToolPendingApproval` fires for High/Critical risk:

1. Push to `output_lines` as an `OutputKind::System` line with a visual
   indicator: `  ⚠ Approve BashExec? [a]yes  [r]no  [s]skip`
2. Keep the existing `ApprovalWidget` modal as a fallback for batch approvals
3. Map `a` key when `approval_queue.has_pending()` to approve (already done)

---

## Acceptance Criteria

1. New messages always appear at the bottom of the visible pane, directly
   above the InputBar separator line.
2. Pressing `↑` once scrolls up by exactly 1 logical line.
3. Pressing `End` or `G` snaps to the absolute newest message.
4. Terminal resize preserves scroll position when `output_auto_scroll=false`.
5. `read_file`/`list_files` show a transient "Reading…" indicator while
   in-flight, replaced by the permanent tool line on completion.
6. Status bar right side shows context percentage when > 70%.
7. Git branch appears in the separator line above the status bar.
8. All existing tests pass. New tests cover scroll step size, resize
   behaviour, and the transient reading indicator.
