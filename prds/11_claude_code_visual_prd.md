# xaft TUI — Claude Code Visual Parity PRD

## Reference

Claude Code TUI output (annotated):

```
● Update the test that expected [coder]:                          ← agent narrative, ● prefix
● Update(/root/rust_projects/xaft/…/conversation.rs)             ← tool call, ● + PascalCase + path
  ⎿  Added 2 lines, removed 1 line                               ← result summary, ⎿ hook
      183          line.agent = Some("coder".into());             ← diff context line (6sp + lineno + content)
      184          let rendered = render_output_line(&line, &theme);
      185          let content: String = rendered.spans.iter().map(|s| s.
           content.as_ref()).collect();                           ← wrapped line: 11sp continuation
      186 -        assert!(content.contains("[coder]"));          ← removed line: lineno + " -" + content
      186 +        // Agent prefix not rendered — phase markers   ← added line
          + identify agent                                        ← wrapped added: "    +" + continuation
      187 +        assert!(!content.contains("[coder]"));
      188          assert!(content.contains("output"));
      189      }
      190  }
● Remove (tool: ...) from planner output:                        ← next narrative chunk
  Read 1 file (ctrl+o to expand)                                 ← collapsible read indicator
● Update(/root/rust_projects/xaft/…/orchestrator.rs)
  ⎿  Added 2 lines, removed 2 lines
✢ Synthesizing… (5m 34s · ↓ 11.4k tokens)                       ← thinking: ✢, verb, elapsed, ↓ tokens
  ⎿  Tip: Use /btw to ask a quick side question                  ← tip line, ⎿ hook
─────────────────────────── fix-streaming-tool-call-demux ──>    ← branch separator
────────────────────────────────────────────────────────  [CAVEMAN]  ← mode separator with mode indicator
  ⏵⏵ bypass permissions on (shift+tab to cycle)                  ← permission/input line
```

---

## Current xaft vs Target

| Visual element | xaft current | Claude Code target |
|---|---|---|
| Agent narrative prefix | none (plain text) | `●` bullet |
| Tool call bullet | `  ◆` (diamond, indented) | `●` (same level as narrative) |
| Tool call name | `ReadFile(path)` | `Read(path)` or `Update(path)` (shorter verbs) |
| Tool result | inline diff lines | `  ⎿  N lines added, M lines removed` |
| Diff block style | `  - old` / `  + new` | line numbers + `-`/`+` + wrapped content |
| File read indicator | `active_agent_thinking` "Reading…" | `  Read N files (ctrl+o to expand)` permanent line |
| Working indicator | `⠄ working…` | `✢ Verb… (Xs · ↓ Nk tokens)` |
| Token direction | `↑ 8.4k` (undifferentiated) | `↓ 11.4k` (↓ = received/output tokens) |
| Bottom bar | 1 separator + 1 text row | 2 separators + 1 input/permission row |
| Mode indicators | none | `[CAVEMAN]` style mode tags on second separator row |
| Tip hints | none | `  ⎿  Tip: …` |

---

## Section 1 — Agent Narrative with `●` Prefix

### Current
Agent text (type `AgentText`) renders as plain text, no prefix.

### Target
```
● Looking at the architecture: the diff viewer needs inline display
  triggered by edit_file/write_file tool completions.
```

First line of each agent "paragraph" is prefixed with `●` (U+25CF, black circle).
Continuation lines (wrapped) align 2 chars from left — no prefix.

### Implementation

In `state.rs`, when `AgentOutput` pushes a new `AgentText` line:
- If the previous `output_lines` entry is NOT `AgentText` (i.e. this is the start of a new agent paragraph), prepend `● ` to the text.
- If previous entry IS `AgentText`, prepend `  ` (2-space indent for continuation).

This means the `●` is embedded in the stored text at push time — no rendering change required.

Edge cases:
- Empty lines between paragraphs: treat next non-empty `AgentText` as new paragraph.
- Tool calls, system lines, diff blocks reset the "paragraph open" flag.

Add `agent_paragraph_open: bool` to `AppState` (reset on any non-AgentText push).

---

## Section 2 — Tool Call Format

### Current
```
  ◆ ReadFile(path/to/file.rs)  [3ms]
```

### Target
```
● Read(path/to/file.rs)
  ⎿  48 lines
```

Changes:
1. Use `●` not `◆`. No indent.
2. Tool name: shorter verb form (see mapping table below).
3. Timing removed from tool call line (move to `⎿` result line).
4. `⎿` result line shows result summary (added below the tool call line on completion).

### Tool name mapping

| Internal name | Display verb |
|---|---|
| `read_file` | `Read` |
| `list_files` | `List` |
| `grep` | `Search` |
| `edit_file` | `Update` |
| `write_file` | `Write` |
| `bash` / `run_command` | `Bash` |
| `create_directory` | `Mkdir` |
| all others | PascalCase of name |

### Result line format (appended on ToolCompleted)

For `read_file`: `  ⎿  {filename}  ({N} lines)`
For `list_files`: `  ⎿  {N} files`
For `grep`: `  ⎿  {N} matches`
For `edit_file` / `write_file`: `  ⎿  {+N lines, -M lines}` (from diff stats)
For `bash`: `  ⎿  exit {code}` or `  ⎿  {first line of stdout}`

### Implementation

In `state.rs`:

1. Change `format_tool_call_inline()` to use verb mapping above and `●` prefix (no indent).
2. On `ToolCompleted`, push a new `OutputKind::ToolResult` line:
   ```
   "  ⎿  " + result_summary
   ```
   where `result_summary` is computed from the tool output or diff stats.
3. Remove timing from the tool call line; optionally add `(Nms)` to the `⎿` result line.
4. Store result metadata in `ToolEntry::result_summary: Option<String>`.

---

## Section 3 — Inline Diff Block Format

### Current
```
  ⎿ -3 +2 (path/to/file.rs)
  - old line A
  - old line B
  + new line A
  + new line B
```

### Target
```
  ⎿  Added 2 lines, removed 1 line
      183          line.agent = Some("coder".into());
      184          let rendered = render_output_line(&line, &theme);
      186 -        assert!(content.contains("[coder]"));
      186 +        // Agent prefix not rendered — phase markers in stream
          + identify agent
      187 +        assert!(!content.contains("[coder]"));
      188          assert!(content.contains("output"));
```

### Format specification

Result summary line: `  ⎿  Added N lines, removed M lines` (use `OutputKind::ToolResult`).

Diff lines (use `OutputKind::ToolResult` for context, `Error` for removed, `Success` for added):

```
      {lineno:>4} {marker} {content}
```

Where:
- `{lineno:>4}` = 4-char right-justified line number, then a space (5 chars total before marker)
- `{marker}` = `-` for removed, `+` for added, ` ` for context
- A space after marker (so: 7 chars before content: `      {4} {m} `)
- Content wraps at terminal width − 7; continuation lines:
  ```
       {5sp}{marker} {continuation}
  ```
  Where 5sp = 5 spaces + marker column to align with content above.

**Context lines** (unchanged): show up to 3 lines before and after each changed hunk.

### Implementation

In `state.rs` `push_inline_file_diff()`:
1. Use `similar` crate (already in Cargo or add it) to generate the diff.
2. Format each hunk using the spec above.
3. Wrap lines at `terminal_size.0 as usize − 7` (dynamic, use stored terminal size).
4. Replace current `"  - "` / `"  + "` format.

Update diff filter in `conversation.rs` (Section 5 Ctrl+O filter) to match the new format:
- `OutputKind::ToolResult` lines starting with `      ` (6 spaces = diff body line).

---

## Section 4 — Collapsible Read Indicator (permanent line)

### Current
`active_agent_thinking` set transiently while read_file is in-flight; cleared on completion. Not persisted.

### Target
After `read_file`/`list_files`/`grep` completes, push a permanent summary line:
```
  Read 1 file (ctrl+o to expand)
```
This line persists in `output_lines` (kind `ToolCall`). When Ctrl+O is pressed and the line's `tool_use_id` is in `collapsed_outputs`, the associated diff lines are hidden; the summary line stays visible.

When Ctrl+O expands:
```
  Read 1 file
  ⎿  path/to/file.rs (48 lines)
```

### Implementation

On `ToolCompleted` for read-type tools:
1. Push permanent `OutputKind::ToolCall` line: `  Read {N} file(s) (ctrl+o to expand)`.
2. If there are detail lines (e.g. file paths from `list_files`), push them as `OutputKind::ToolResult` lines tagged with `tool_use_id`.
3. `collapsed_outputs: HashSet<String>` gates visibility of tagged detail lines.

Replace the `show_diff_inline: bool` global toggle with `collapsed_outputs: HashSet<String>` (per-tool).
Ctrl+O: toggle the nearest upstream `tool_use_id` from current scroll position.

To support `tool_use_id` filtering, add `tool_use_id: Option<String>` field to `OutputLine`.

---

## Section 5 — Working Indicator

### Current
```
⠄ working…
```
Shows in `InputBar` when `phase.is_active()`.

### Target
```
✢ Synthesizing… (5m 34s · ↓ 11.4k tokens)
```

Changes:
1. Replace braille spinner with `✢` (U+2722, four teardrop-spoked asterisk) — static or rotating through `✢ ✣ ✤ ✥`.
2. Show task-specific verb instead of "working":
   - Planning phase: `Planning…`
   - Coding phase: `Synthesizing…` / `Coding…` / `Thinking…` (rotate through on Tick)
   - QA phase: `Reviewing…`
   - Fixing phase: `Fixing…`
3. Elapsed time: already implemented (Section 3 of PRD 10). Move from `active_agent_thinking` to InputBar.
4. Token direction arrow: `↓` for output tokens received (agent response), `↑` for input tokens sent. Use `total_output_tokens` for `↓`, `total_input_tokens` for `↑`.

### Format
```
✢ {Verb}… ({elapsed} · ↓ {output_tok})
```
Show in `InputBar` content area (same as current spinner). Move elapsed+token display OUT of `active_agent_thinking` (which should only show the last streamed text excerpt).

---

## Section 6 — Bottom Bar Layout (3 rows)

### Current (2 rows when layout allocates height=2)
```
Row 0: ──────────── fix-streaming-tool-call-demux ──>   ← branch separator
Row 1: xaft · 20.1k tok · $0.0000 · Coder  [↑↓] scroll [Esc] cancel [q] quit
```

### Target (3 rows)
```
Row 0: ─────────────────── fix-streaming-tool-call-demux ──>   ← branch separator
Row 1: ──────────────────────────────────────────  [CAVEMAN]   ← mode separator
Row 2:   ⏵⏵ bypass permissions on (shift+tab to cycle)         ← input/permission line
```

In xaft terms:
```
Row 0: ─────────────────── {git_branch} ──>              ← branch separator (Section 6 of PRD 10)
Row 1: ───────────────────────────────  {mode_tags}       ← token/cost/phase info as muted tags
Row 2:   {spinner} {verb}… ({elapsed} · {tokens})         ← InputBar content
```

Where `{mode_tags}` = `  xaft  ·  {tok}  ·  {cost}  ·  {phase}` right-aligned against the `─` line.

### Implementation

1. **Layout**: increase `StatusBar` min-height from 2 to 3 in `layout.rs`.
2. **StatusBar** renders:
   - Row 0: branch separator (current)
   - Row 1: `─` separator across full width, then RIGHT-align `  xaft · tok · cost · phase  ` into the right portion (overwriting `─` chars with text)
   - Row 2: empty (left for InputBar to overwrite from above, OR show approval keys if `approval_queue.has_pending()`)
3. **InputBar** shrinks by 1 row (its top separator is now row 2 of StatusBar's area). OR: InputBar takes over row 2 of StatusBar area entirely.

Simplest approach: Give InputBar height=3 (top-sep + content + bottom-sep) and StatusBar height=3 (row0=branch, row1=info+sep, row2=absorbed by InputBar top).

Actually cleaner: make the InputBar's top `─` row serve double duty as StatusBar's info row. StatusBar renders rows 0+1; InputBar starts at row 1 (its top sep is row 1, overwriting StatusBar's row 1 sep). Then:
- StatusBar row 0: branch
- StatusBar row 1 / InputBar row 0: `─` line with tok/cost/phase info right-aligned
- InputBar row 1: spinner content
- InputBar row 2: bottom `─`

This requires coordination between the two widgets. Cleaner alternative: combine into a single `BottomBar` widget (rows 0–3) that renders all four rows.

---

## Section 7 — Tip / Hint Lines

### Target
```
  ⎿  Tip: Use /btw to ask a quick side question without interrupting Claude's current work
```

Shown below the last agent text paragraph when the agent is idle (between turns).

### Implementation

In `state.rs`, after `AgentRunComplete`:
1. Select a tip from a static list (10–20 tips).
2. Push `OutputKind::System` line: `  ⎿  Tip: {text}`.
3. Tips rotate (cycle index stored in `AppState`, incremented each run).

Static tip list examples:
- `Use /clear to start a new task`
- `Press q to cancel the current run`
- `Scroll up with ↑ to review earlier output`
- `Press Ctrl+O to collapse long tool outputs`

---

## Section 8 — Agent Name in Thinking Line

### Current
```
  ⋯  (5s · ↑ 8.4k)
```

### Target
```
✢ Synthesizing… (5m 34s · ↓ 11.4k tokens)
```

`active_agent_thinking` should show last streamed text excerpt ONLY, not the elapsed+token line.
Elapsed+token moves to InputBar working indicator (Section 5).

Keep `active_agent_thinking` for the streamed excerpt: last 80 chars of last `AgentOutput` line, prefixed `  ` (2-space indent, no bullet).

---

## Acceptance Criteria

1. Every agent narrative line starts with `●` (new paragraph) or `  ` (continuation).
2. Tool calls use `●` prefix + short verb + primary arg, no indent, no timing on that line.
3. `⎿` result line appears below each tool call on completion with result summary.
4. Diff blocks show line numbers + `+`/`-` markers + wrapped content aligned to column 7.
5. Read-type tools push permanent `  Read N file(s) (ctrl+o to expand)` line.
6. Working indicator shows `✢ {Verb}… ({elapsed} · ↓ {Nk} tokens)` in InputBar.
7. Bottom bar has 3 visible rows: branch separator / info+separator / spinner content.
8. Tip lines appear after `AgentRunComplete` as `  ⎿  Tip: …`.
9. All existing tests pass. New tests cover: `●` prefix logic, tool verb mapping, diff line format, tip rotation.

---

## Files to Change

| File | Changes |
|---|---|
| `state.rs` | `agent_paragraph_open`, tool verb mapping, `⎿` result push, tip push, `OutputLine::tool_use_id`, `collapsed_outputs: HashSet<String>` |
| `widgets/conversation.rs` | Remove agent-prefix span (done), update Ctrl+O filter for new diff format |
| `widgets/status_bar.rs` | 3-row layout: branch / info+sep / empty |
| `widgets/input_bar.rs` | `✢` indicator, phase verb, token direction |
| `layout.rs` | StatusBar min-height = 3 (or combined BottomBar) |
| `orchestrator.rs` | — |
